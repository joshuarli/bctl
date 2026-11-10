use std::thread;
use std::time::{Duration, Instant};

use crate::host::{self, Fallible, Runtime, footprint_mib, output_of, process_stats};
use crate::report::{Samples, fact};
use crate::session::{Home, session_name};
use crate::stats::Statistic;
use crate::{Context, Outcome};

const SETTLE: Duration = Duration::from_secs(30);
const IDLE_WINDOW: Duration = Duration::from_secs(60);

fn kib_to_mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn tree_stats(pids: &[u32]) -> Fallible<(u64, f64)> {
    let mut rss = 0;
    let mut cpu = 0.0;
    for pid in pids {
        let stats = process_stats(*pid)?;
        rss += stats.rss_kib;
        cpu += stats.cpu_seconds;
    }
    Ok((rss, cpu))
}

fn disk_kib(path: &str) -> Fallible<u64> {
    output_of("du", &["-sk", path])?.split_whitespace().next().and_then(|v| v.parse().ok()).ok_or_else(|| format!("du printed nothing for {path}"))
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let runtime = Runtime::current()?;
    let image = runtime.image();
    let mut outcome = Outcome { images: vec![image], ..Outcome::default() };
    let root = host::root();
    let bctl = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("bctl");
    let binary_bytes = std::fs::metadata(&bctl).map_err(|e| format!("{}: {e}", bctl.display()))?.len();
    outcome.measures.push(fact("resources.bctl_binary", "bytes", binary_bytes as f64, "target/release/bctl as built by cargo build --release -p bctl, unstripped, default release profile"));
    match runtime {
        Runtime::SmolVm => {
            if let Ok(meta) = std::fs::metadata(root.join(format!(".work/images/{image}.tar"))) {
                outcome.measures.push(fact(&format!("resources.image_tar.{image}"), "bytes", meta.len() as f64, "docker save archive handed to smolvm"));
            }
        }
        Runtime::Nanos => {
            let disk = root.join(format!(".work/images/{image}.img"));
            let meta = std::fs::metadata(&disk).map_err(|e| format!("{}: {e}", disk.display()))?;
            outcome.measures.push(fact(&format!("resources.image_disk.{image}"), "bytes", meta.len() as f64, "Nanos disk image (2 GiB sparse)"));
            outcome.measures.push(fact(&format!("resources.image_disk_allocated.{image}"), "KiB", disk_kib(disk.to_str().expect("utf-8 path"))? as f64, "du -k of the Nanos disk image, allocated blocks only"));
        }
    }

    let mut daemon_empty = Samples::new();
    let mut daemon_with_session = Samples::new();
    let mut daemon_cpu = Samples::new();
    let mut per_image = vec![(image, [Samples::new(), Samples::new(), Samples::new(), Samples::new(), Samples::new(), Samples::new()])];

    for round in 0..ctx.rounds {
        for (image, [vmm_rss, vmm_rss_after, vmm_cpu, disk, profile, vmm_footprint]) in &mut per_image {
            let home = Home::new("res")?;
            home.start_daemon()?;
            thread::sleep(Duration::from_secs(2));
            daemon_empty.push(kib_to_mib(process_stats(home.daemon_pid()?)?.rss_kib));
            let name = session_name("res");
            if let Err(e) = home.up(&name, &name, image, &[]) {
                outcome.notes.push(format!("{image} round {round}: up failed: {e}"));
                continue;
            }
            let machine = home.session(&name)?.machine.ok_or("ready session has no machine")?;
            thread::sleep(SETTLE);
            let vmm = home.vmm_pids()?;
            if vmm.is_empty() {
                return Err(format!("no VMM process found under {}", home.path.display()));
            }
            let daemon_pid = home.daemon_pid()?;
            vmm_rss.push(kib_to_mib(tree_stats(&vmm)?.0));
            vmm_footprint.push(vmm.iter().map(|pid| footprint_mib(*pid)).sum::<Fallible<f64>>()?);
            daemon_with_session.push(kib_to_mib(process_stats(daemon_pid)?.rss_kib));

            if ctx.gate.gave_up() {
                outcome.notes.push(format!("perf gate deferred: host busy; idle CPU window skipped for {image} round {round}"));
            } else {
                let window = ctx.gate.run(|| {
                    let (_, vmm_before) = tree_stats(&vmm)?;
                    let (_, daemon_before) = tree_stats(&[daemon_pid])?;
                    let started = Instant::now();
                    thread::sleep(IDLE_WINDOW);
                    let elapsed = started.elapsed().as_secs_f64();
                    let (_, vmm_after) = tree_stats(&vmm)?;
                    let (_, daemon_after) = tree_stats(&[daemon_pid])?;
                    Ok(((vmm_after - vmm_before) / elapsed * 100.0, (daemon_after - daemon_before) / elapsed * 100.0))
                })?;
                if let Some(((vmm_percent, daemon_percent), _, verdict)) = window {
                    eprintln!("resources round {round}: {image} idle vmm {vmm_percent:.2}% daemon {daemon_percent:.2}% of one core ({verdict:?})");
                    vmm_cpu.record(vmm_percent, verdict);
                    daemon_cpu.record(daemon_percent, verdict);
                    vmm_rss_after.push(kib_to_mib(tree_stats(&vmm)?.0));
                } else {
                    outcome.notes.push(format!("perf gate deferred: host busy; idle CPU window skipped for {image} round {round}"));
                }
            }

            match runtime {
                Runtime::SmolVm => {
                    let data_dir = output_of(home.smolvm().to_str().expect("utf-8 path"), &["machine", "data-dir", "--name", &machine])?;
                    disk.push(disk_kib(&data_dir)? as f64 / 1024.0);
                }
                Runtime::Nanos => {
                    let machine_dir = home.path.join("machines").join(&machine);
                    disk.push(disk_kib(machine_dir.to_str().expect("utf-8 path"))? as f64 / 1024.0);
                }
            }
            profile.push(disk_kib(home.path.join("profiles").to_str().expect("utf-8 path"))? as f64 / 1024.0);
        }
    }

    for (image, [vmm_rss, vmm_rss_after, vmm_cpu, disk, profile, vmm_footprint]) in &per_image {
        let summed = match runtime {
            Runtime::SmolVm => "summed over the VMM process and its video encoder helper",
            Runtime::Nanos if cfg!(target_os = "macos") => "summed over libkrun and the bctl nanos-vm supervisor",
            Runtime::Nanos => "summed over qemu, vhost-device-vsock and the bctl nanos-vm supervisor",
        };
        let conditions = format!("{image}, 2 CPUs, 4096 MiB guest, 1280x800; {summed}");
        let footprint = if cfg!(target_os = "linux") { "PSS from /proc/<pid>/smaps_rollup" } else { "macOS physical footprint (includes compressed pages)" };
        let machine_disk = match runtime {
            Runtime::SmolVm => "du of the machine data directory after ready; sparse files count only allocated blocks",
            Runtime::Nanos => "du of the machine directory after ready; sparse files count only allocated blocks",
        };
        outcome.measures.push(vmm_rss.finish(&format!("resources.vmm_rss.{image}"), "MiB", Statistic::Median, &format!("{conditions}; {} s after ready, browser on about:blank", SETTLE.as_secs())));
        outcome.measures.push(vmm_footprint.finish(&format!("resources.vmm_footprint.{image}"), "MiB", Statistic::Median, &format!("{conditions}; {footprint}, same moment as vmm_rss")));
        outcome.measures.push(vmm_rss_after.finish(&format!("resources.vmm_rss_after_idle.{image}"), "MiB", Statistic::Median, &format!("{conditions}; after a further {} s idle", IDLE_WINDOW.as_secs())));
        outcome.measures.push(vmm_cpu.finish(&format!("resources.vmm_idle_cpu.{image}"), "percent of one core", Statistic::Median, &format!("{conditions}; CPU time over a {} s window starting {} s after ready, from process CPU time", IDLE_WINDOW.as_secs(), SETTLE.as_secs())));
        outcome.measures.push(disk.finish(&format!("resources.machine_disk.{image}"), "MiB", Statistic::Median, machine_disk));
        outcome.measures.push(profile.finish(&format!("resources.profile_disk.{image}"), "MiB", Statistic::Median, "du of the profiles directory after ready, before any browsing"));
    }
    outcome.measures.push(daemon_empty.finish("resources.daemon_rss.no_session", "MiB", Statistic::Median, "bctl daemon 2 s after start, no session"));
    outcome.measures.push(daemon_with_session.finish("resources.daemon_rss.one_session", "MiB", Statistic::Median, "bctl daemon with one ready session, 30 s after ready"));
    outcome.measures.push(daemon_cpu.finish("resources.daemon_idle_cpu.one_session", "percent of one core", Statistic::Median, "bctl daemon with one ready session, 60 s window"));
    Ok(outcome)
}
