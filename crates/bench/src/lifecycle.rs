use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use crate::host::{Fallible, output_of};
use crate::report::{Samples, fact};
use crate::session::{Home, session_name};
use crate::stats::Statistic;
use crate::{Context, Outcome};

const IMAGE: &str = "ref-alpine-chromium";
const STAGES: [&str; 8] = ["machine", "guest", "seat", "compositor", "browser", "sandbox", "cdp", "display"];

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let mut outcome = Outcome { images: vec![IMAGE], ..Outcome::default() };
    let mut daemon = Samples::new();
    let mut cold_total = Samples::new();
    let mut cold_stages: BTreeMap<&str, Samples> = STAGES.iter().map(|s| (*s, Samples::new())).collect();
    let mut cold_first = Samples::new();
    let mut warm_first = Samples::new();
    let mut cold_rest = Samples::new();
    let mut warm_rest = Samples::new();
    let mut warm_total = Samples::new();
    let mut warm_stages: BTreeMap<&str, Samples> = STAGES.iter().map(|s| (*s, Samples::new())).collect();
    let mut down = Samples::new();
    let mut browser = None;

    for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; stopped before round {round} of {}", ctx.rounds));
            break;
        }
        let home = Home::new("life")?;
        let (spawn, _, verdict) = ctx.gate.run(|| home.start_daemon())?;
        daemon.record(ms(spawn), verdict);

        let name = session_name("life");
        let profile = format!("h1-life-{round}");
        for (label, total, stages, first, rest) in [("cold", &mut cold_total, &mut cold_stages, &mut cold_first, &mut cold_rest), ("warm", &mut warm_total, &mut warm_stages, &mut warm_first, &mut warm_rest)] {
            let (timing, _, verdict) = ctx.gate.run(|| home.up(&name, &profile, IMAGE, &[]))?;
            total.record(ms(timing.total), verdict);
            if let Some((_, at)) = timing.stages.first() {
                first.record(ms(*at), verdict);
                rest.record(ms(timing.total.saturating_sub(*at)), verdict);
            }
            for (stage, at) in &timing.stages {
                if let Some(samples) = stages.get_mut(stage.as_str()) {
                    samples.record(ms(*at), verdict);
                }
            }
            eprintln!("lifecycle round {round}: {label} up {:.0} ms ({verdict:?})", ms(timing.total));
            if label == "warm" && browser.is_none() {
                browser = Some(crate::cdp_version_of(&home, &name)?);
            }
            if label == "cold" && round == 0 {
                outcome.notes.push(pause_on_appliance(&home, &name)?);
            }
            let (stopped, _, verdict) = ctx.gate.run(|| home.down(&name))?;
            down.record(ms(stopped), verdict);
        }
        home.remove(&name)?;
    }

    let conditions = "ref-alpine-chromium, 2 CPUs, 4096 MiB, 1280x800, fresh daemon and profile for cold; smolvm image cache and OS page cache warm because the host is shared and cannot be purged";
    outcome.measures.push(daemon.finish("lifecycle.daemon_start", "ms", Statistic::Median, "first bctl command on an empty home: spawn the daemon until it answers"));
    outcome.measures.push(cold_total.finish("lifecycle.cold_up.total", "ms", Statistic::Median, conditions));
    outcome.measures.push(cold_first.finish("lifecycle.cold_up.first_probe", "ms", Statistic::Median, "time after the up request at which status first named any stage, i.e. create and start of the machine until the first readiness probe fails"));
    outcome.measures.push(cold_rest.finish("lifecycle.cold_up.after_first_probe", "ms", Statistic::Median, "total minus first_probe: every readiness stage after the first failed probe, through the display having drawn"));
    for stage in STAGES {
        let samples = cold_stages.remove(stage).expect("stage exists");
        outcome.measures.push(samples.finish(&format!("lifecycle.cold_up.first_seen.{stage}"), "ms", Statistic::Median, "time after the up request at which status first named this stage as the one not yet reached; a stage that passes between 20 ms polls is not seen"));
    }
    outcome.measures.push(warm_total.finish("lifecycle.warm_up.total", "ms", Statistic::Median, "same session and profile as the cold up, after down; image and profile warm"));
    outcome.measures.push(warm_first.finish("lifecycle.warm_up.first_probe", "ms", Statistic::Median, "as cold_up.first_probe, on the warm up"));
    outcome.measures.push(warm_rest.finish("lifecycle.warm_up.after_first_probe", "ms", Statistic::Median, "as cold_up.after_first_probe, on the warm up"));
    for stage in STAGES {
        let samples = warm_stages.remove(stage).expect("stage exists");
        outcome.measures.push(samples.finish(&format!("lifecycle.warm_up.first_seen.{stage}"), "ms", Statistic::Median, "as cold_up, on the warm up"));
    }
    outcome.measures.push(down.finish("lifecycle.down", "ms", Statistic::Median, "bctl down of a ready session, after both the cold and the warm up"));
    outcome.browser_versions.extend(browser);
    bare_pause_resume(ctx, &mut outcome)?;
    Ok(outcome)
}

fn pause_on_appliance(home: &Home, session: &str) -> Fallible<String> {
    let machine = home.session(session)?.machine.ok_or("ready session has no machine")?;
    let output = Command::new(home.smolvm()).args(["machine", "pause", "--name", &machine]).output().map_err(|e| e.to_string())?;
    let still_running = home.session(session)?.state == "ready";
    Ok(format!(
        "smolvm machine pause on the daemon-owned appliance machine: exit {}, stderr {:?}; session still ready: {still_running}",
        output.status.code().map_or("signal".into(), |c| c.to_string()),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

struct BareMachine {
    smolvm: std::path::PathBuf,
    name: String,
}

impl BareMachine {
    fn smolvm(&self, args: &[&str]) -> Fallible<String> {
        output_of(self.smolvm.to_str().expect("utf-8 path"), args)
    }

    fn exec(&self, script: &str) -> Fallible<String> {
        self.smolvm(&["machine", "exec", "--name", &self.name, "--", "sh", "-c", script])
    }
}

impl Drop for BareMachine {
    fn drop(&mut self) {
        let _ = self.smolvm(&["machine", "stop", "--name", &self.name]);
        let _ = self.smolvm(&["machine", "delete", "--name", &self.name, "-f"]);
    }
}

fn bare_pause_resume(ctx: &Context, outcome: &mut Outcome) -> Fallible<()> {
    let home = Home::new("pause")?;
    let machine = BareMachine { smolvm: home.smolvm(), name: session_name("pause-bare") };
    machine.smolvm(&["machine", "create", "--name", &machine.name, "--mem", "4096", "--cpus", "2"])?;
    machine.smolvm(&["machine", "start", "--name", &machine.name])?;
    machine.exec("nohup sh -c 'i=0; while :; do i=$((i+1)); echo $i > /tmp/counter; sleep 0.2; done' >/dev/null 2>&1 & echo $! > /tmp/counter.pid")?;
    let mut pause = Samples::new();
    let mut resume = Samples::new();
    let mut survived = 0;
    for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; pause and resume stopped before round {round} of {}", ctx.rounds));
            break;
        }
        let pid_before = machine.exec("cat /tmp/counter.pid")?;
        let before: u64 = machine.exec("cat /tmp/counter")?.parse().map_err(|_| "counter is not a number")?;
        let (_, paused, verdict) = ctx.gate.run(|| machine.smolvm(&["machine", "pause", "--name", &machine.name]))?;
        pause.record(ms(paused), verdict);
        let (_, resumed, verdict) = ctx.gate.run(|| machine.smolvm(&["machine", "resume", "--name", &machine.name]))?;
        resume.record(ms(resumed), verdict);
        std::thread::sleep(Duration::from_secs(1));
        let alive = machine.exec(&format!("kill -0 {pid_before} && cat /tmp/counter"))?;
        let after: u64 = alive.lines().last().and_then(|l| l.parse().ok()).unwrap_or(0);
        if after > before {
            survived += 1;
        }
    }
    outcome.measures.push(pause.finish("lifecycle.pause_bare", "ms", Statistic::Median, "smolvm machine pause of a bare machine (no image, no GPU, no exposed socket), 4096 MiB, 2 CPUs, idle; the appliance configuration is refused, see notes"));
    outcome.measures.push(resume.finish("lifecycle.resume_bare", "ms", Statistic::Median, "smolvm machine resume of the same bare machine"));
    outcome.measures.push(fact("lifecycle.bare_state_survived", "rounds", f64::from(survived), &format!("rounds out of {} in which a guest process kept its pid and its counter kept advancing after resume", ctx.rounds)));
    Ok(())
}
