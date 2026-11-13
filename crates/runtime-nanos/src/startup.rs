use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) struct Startup {
    started: Option<Instant>,
    component: &'static str,
}

impl Startup {
    pub(crate) fn new(component: &'static str) -> Self {
        Self { started: std::env::var_os("BCTL_STARTUP_TRACE").map(|_| Instant::now()), component }
    }

    pub(crate) fn mark(&self, stage: &str) {
        if let Some(started) = self.started {
            let epoch = match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(duration) => duration.as_micros(),
                Err(error) => { eprintln!("bctl-startup: system clock: {error}"); return; }
            };
            eprintln!("bctl-startup {} {stage} epoch_us={epoch} elapsed_us={}", self.component, started.elapsed().as_micros());
        }
    }
}
