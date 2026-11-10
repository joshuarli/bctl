use std::path::Path;
use std::time::{Duration, Instant};

use bctl_rfb::Client;
use bctl_core::{Endpoints, Generation, MachineName, SessionName};
use bctl_runtime::cdp_socket_browser;
use bctl_runtime_smolvm::{LaunchSource, LaunchSpec, SmolVm, TemplateSpec};

use crate::host::{self, Fallible};
use crate::report::{Samples, fact};
use crate::stats::Statistic;
use crate::{Context, Outcome};

const IMAGE: &str = "helium";

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn load() -> Fallible<String> {
    host::output_of("sysctl", &["-n", "vm.loadavg"])
}

fn free_port() -> Fallible<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

#[derive(Clone, Copy)]
enum Kind {
    Cold,
    Template,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Cold => "cold",
            Kind::Template => "template",
        }
    }

    fn source(self, image: &Path, checkpoint: &Path) -> LaunchSource {
        match self {
            Kind::Cold => LaunchSource::Image(image.to_path_buf()),
            Kind::Template => LaunchSource::Checkpoint(checkpoint.to_path_buf()),
        }
    }
}

fn template_spec(root: &Path) -> TemplateSpec {
    let scratch = root.join(".work/tmp");
    let cdp = scratch.join(format!("x1-bench-capture-cdp-{}", std::process::id()));
    TemplateSpec {
        machine: format!("x1-bench-template-{}", std::process::id()),
        image: root.join(".work/images").join(format!("{IMAGE}.tar")),
        checkpoint: scratch.join(format!("x1-bench-template-{}.checkpoint", std::process::id())),
        staging_dir: scratch.join(format!("x1-bench-staging-{}", std::process::id())),
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
        cdp_socket: cdp.join("s"),
    }
}

fn wait_cdp_and_frame(spec: &LaunchSpec) -> Fallible<()> {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if cdp_socket_browser(&spec.cdp_socket).is_some()
            && let Ok(mut vnc) = Client::connect(("127.0.0.1", spec.endpoints.vnc_port), Duration::from_secs(5))
            && let Ok(frame) = vnc.refresh()
        {
            let black = bctl_rfb::Rgb(0, 0, 0);
            if (0..frame.height()).step_by(16).any(|y| (0..frame.width()).step_by(16).any(|x| frame.pixel(x, y) != black)) {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err("no CDP answer and non-black frame within 180 s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn launch(runtime: &SmolVm, root: &Path, kind: Kind, round: usize, checkpoint: &Path) -> Fallible<Duration> {
    let tag = format!("{round}-{}", kind.label());
    let session = SessionName::new(&format!("x1-{tag}-{}", std::process::id())).map_err(|e| e.to_string())?;
    let cdp = root.join(format!(".work/tmp/x1-bench-cdp-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&cdp).map_err(|e| e.to_string())?;
    let spec = LaunchSpec {
        machine: MachineName { session, generation: Generation(1) },
        source: kind.source(&root.join(".work/images").join(format!("{IMAGE}.tar")), checkpoint),
        profile_dir: None,
        endpoints: Endpoints { vnc_port: free_port()? },
        cdp_socket: cdp.join("s"),
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
        allow_private: Vec::new(),
    };
    let started = Instant::now();
    let result = runtime.launch(&spec).map_err(|e| e.to_string()).and_then(|()| wait_cdp_and_frame(&spec));
    let elapsed = started.elapsed();
    let removed = runtime.remove(&spec.machine).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&cdp);
    result?;
    removed?;
    Ok(elapsed)
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let root = host::root();
    let mut outcome = Outcome { images: vec![IMAGE], ..Outcome::default() };
    let spec = template_spec(&root);
    let runtime = SmolVm::new(root.join(".work/runtime/smolvm/smolvm"));
    let captured = Instant::now();
    runtime.create_template(&spec).map_err(|e| e.to_string())?;
    let captured_for = captured.elapsed();
    let bytes = std::fs::metadata(&spec.checkpoint).map_err(|e| format!("{}: {e}", spec.checkpoint.display()))?.len();
    outcome.notes.push(format!(
        "template captured from {} in {:.0} ms at load {} ({})",
        spec.image.display(),
        ms(captured_for),
        load()?,
        spec.checkpoint.display()
    ));
    outcome.measures.push(fact("restore.template.capture", "ms", ms(captured_for), "smolvm machine checkpoint of the parked pre-browser guest, one capture"));
    outcome.measures.push(fact("restore.template.checkpoint_bytes", "bytes", bytes as f64, "standalone portable checkpoint file"));

    for kind in [Kind::Cold, Kind::Template] {
        let elapsed = launch(&runtime, &root, kind, 0, &spec.checkpoint)?;
        let load = load()?;
        outcome.notes.push(format!("warm-up {}: launch to CDP ready and first frame {:.0} ms at load {load}", kind.label(), ms(elapsed)));
    }

    let mut cold = Samples::new();
    let mut restored = Samples::new();
    'rounds: for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; stopped before round {round} of {}", ctx.rounds));
            break;
        }
        let order = if round % 2 == 0 { [Kind::Cold, Kind::Template] } else { [Kind::Template, Kind::Cold] };
        for kind in order {
            let Some((elapsed, gate_elapsed, verdict)) = ctx.gate.run(|| launch(&runtime, &root, kind, round + 1, &spec.checkpoint))? else {
                outcome.notes.push(format!("perf gate deferred: host busy; stopped during round {round}"));
                break 'rounds;
            };
            let load = load()?;
            outcome.notes.push(format!("round {round} {}: launch {:.0} ms (gate {:.0} ms), load {load}, {verdict:?}", kind.label(), ms(elapsed), ms(gate_elapsed)));
            match kind {
                Kind::Cold => cold.record(ms(elapsed), verdict),
                Kind::Template => restored.record(ms(elapsed), verdict),
            }
        }
    }

    let conditions = "helium, 2 CPUs, 4096 MiB, 1280x800, fresh machine per sample, image cache warm; the timed region is SmolVm::launch to CDP answering plus a first non-black VNC frame; every sample's one-minute load is in the notes";
    outcome.measures.push(cold.finish("restore.cold_launch.total", "ms", Statistic::Median, conditions));
    outcome.measures.push(restored.finish("restore.template_launch.total", "ms", Statistic::Median, conditions));
    Ok(outcome)
}
