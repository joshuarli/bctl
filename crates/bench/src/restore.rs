use std::time::Duration;

use bctl_runtime_smolvm::{SmolVm, TemplateSpec};

use crate::host::{self, Fallible};
use crate::report::{Samples, fact};
use crate::session::Home;
use crate::stats::Statistic;
use crate::{Context, Outcome};

const IMAGE: &str = "helium";
const HOST_RANGE: &str = "100.96.0.0/30";
const TEMPLATE: &str = "BCTL_TEMPLATE_CHECKPOINT";

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn load() -> Fallible<String> {
    host::output_of("sysctl", &["-n", "vm.loadavg"])
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
}

struct Sample {
    home: Home,
    name: String,
    env: Vec<(&'static str, String)>,
}

impl Sample {
    fn start(round: usize, kind: Kind, checkpoint: &str) -> Fallible<Sample> {
        let tag = format!("{round}-{}", kind.label());
        let home = Home::new(&format!("x1-{tag}"))?;
        let env: Vec<(&'static str, String)> = match kind {
            Kind::Cold => Vec::new(),
            Kind::Template => vec![(TEMPLATE, checkpoint.to_string())],
        };
        home.start_daemon_with_env(&env)?;
        Ok(Sample { home, name: format!("x1-{tag}"), env })
    }

    fn up(&self) -> Fallible<Duration> {
        let name = self.name.as_str();
        let profile = self.name.as_str();
        Ok(self.home.up_with_env(name, profile, IMAGE, &[HOST_RANGE.to_string()], &self.env)?.total)
    }

    fn finish(self) -> Fallible<()> {
        self.home.down(&self.name)?;
        self.home.remove(&self.name)?;
        Ok(())
    }
}

fn template_spec(root: &std::path::Path) -> TemplateSpec {
    let scratch = root.join(".work/tmp");
    let cdp = scratch.join(format!("x1-bench-cdp-{}", std::process::id()));
    TemplateSpec {
        machine: format!("h1-x1-template-{}", std::process::id()),
        image: root.join(".work/images").join(format!("{IMAGE}.tar")),
        checkpoint: scratch.join(format!("x1-bench-template-{}.checkpoint", std::process::id())),
        staging_dir: scratch.join(format!("x1-bench-staging-{}", std::process::id())),
        display: (1280, 800),
        memory_mib: 4096,
        cpus: 2,
        cdp_socket: cdp.join("s"),
    }
}

pub fn run(ctx: &Context) -> Fallible<Outcome> {
    let root = host::root();
    let mut outcome = Outcome { images: vec![IMAGE], ..Outcome::default() };
    let spec = template_spec(&root);
    let runtime = SmolVm::new(root.join(".work/runtime/smolvm/smolvm"));
    let captured = std::time::Instant::now();
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
    let checkpoint = spec.checkpoint.to_string_lossy().into_owned();

    for kind in [Kind::Cold, Kind::Template] {
        let sample = Sample::start(0, kind, &checkpoint)?;
        let elapsed = sample.up()?;
        let load = load()?;
        sample.finish()?;
        outcome.notes.push(format!("warm-up {}: up {:.0} ms at load {load}", kind.label(), ms(elapsed)));
    }

    let mut cold = Samples::new();
    let mut restored = Samples::new();
    for round in 0..ctx.rounds {
        if ctx.gate.gave_up() {
            outcome.notes.push(format!("perf gate deferred: host busy; stopped before round {round} of {}", ctx.rounds));
            break;
        }
        let order = if round % 2 == 0 { [Kind::Cold, Kind::Template] } else { [Kind::Template, Kind::Cold] };
        for kind in order {
            let sample = Sample::start(round + 1, kind, &checkpoint)?;
            let (up, elapsed, verdict) = ctx.gate.run(|| sample.up())?;
            let load = load()?;
            sample.finish()?;
            outcome.notes.push(format!("round {round} {}: up {:.0} ms (gate {:.0} ms), load {load}, {verdict:?}", kind.label(), ms(up), ms(elapsed)));
            match kind {
                Kind::Cold => cold.record(ms(up), verdict),
                Kind::Template => restored.record(ms(up), verdict),
            }
        }
    }

    let conditions = "helium, 2 CPUs, 4096 MiB, 1280x800, fresh daemon and profile per sample, image cache warm; every sample's one-minute load is in the notes";
    outcome.measures.push(cold.finish("restore.cold_up.total", "ms", Statistic::Median, conditions));
    outcome.measures.push(restored.finish("restore.template_up.total", "ms", Statistic::Median, conditions));
    Ok(outcome)
}
