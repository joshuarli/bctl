use std::collections::BTreeMap;
use std::time::Duration;

use crate::host::{Fallible, Runtime};
use crate::report::{Samples, fact};
use crate::session::{Home, session_name};
use crate::stats::Statistic;
use crate::{Context, Outcome};

const STAGES: [&str; 8] = ["machine", "guest", "seat", "compositor", "browser", "sandbox", "cdp", "display"];

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let runtime = Runtime::current()?;
    let image = runtime.image();
    let mut outcome = Outcome { images: vec![image], ..Outcome::default() };
    if runtime == Runtime::Nanos {
        outcome.notes.push("pause and resume are not supported by the nanos runtime".into());
    }
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
    let mut pause = Samples::new();
    let mut resume = Samples::new();
    let mut kept = 0;
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
            let (timing, _, verdict) = ctx.gate.run(|| home.up(&name, &profile, image, &[]))?;
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
            if label == "cold" && runtime == Runtime::SmolVm {
                let title = format!("kept-{round}");
                home.open_tab(&name, &format!("data:text/html,<title>{title}</title>"))?;
                let (paused, _, verdict) = ctx.gate.run(|| home.pause(&name))?;
                pause.record(ms(paused), verdict);
                let (resumed, _, verdict) = ctx.gate.run(|| home.resume(&name))?;
                resume.record(ms(resumed), verdict);
                if home.page(&["get", &name, "title"])? == title {
                    kept += 1;
                }
            }
            let (stopped, _, verdict) = ctx.gate.run(|| home.down(&name))?;
            down.record(ms(stopped), verdict);
        }
        home.remove(&name)?;
    }

    let conditions = format!("{} image {image}, 2 CPUs, 4096 MiB, 1280x800, fresh daemon and profile for cold; the {image} image cache and OS page cache are warm because the host is shared and cannot be purged", runtime.name());
    outcome.measures.push(daemon.finish("lifecycle.daemon_start", "ms", Statistic::Median, "first bctl command on an empty home: spawn the daemon until it answers"));
    outcome.measures.push(cold_total.finish("lifecycle.cold_up.total", "ms", Statistic::Median, &conditions));
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
    if runtime == Runtime::SmolVm {
        outcome.measures.push(pause.finish("lifecycle.pause", "ms", Statistic::Median, "bctl pause of a ready session after a page load, until the VM is checkpointed in place"));
        outcome.measures.push(resume.finish("lifecycle.resume", "ms", Statistic::Median, "bctl resume of the paused session until it is ready again"));
        outcome.measures.push(fact("lifecycle.resume_kept_page", "rounds", f64::from(kept), &format!("rounds out of {} in which the page loaded before pause still had its title after resume", ctx.rounds)));
    }
    outcome.browser_versions.extend(browser);
    Ok(outcome)
}
