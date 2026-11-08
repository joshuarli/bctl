use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

pub struct Shutdown {
    signal: Arc<AtomicUsize>,
    registrations: Vec<signal_hook::SigId>,
}

impl Shutdown {
    pub fn new() -> std::io::Result<Self> {
        let mut shutdown = Self { signal: Arc::new(AtomicUsize::new(0)), registrations: Vec::new() };
        for signal in [signal_hook::consts::SIGHUP, signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM, signal_hook::consts::SIGQUIT] {
            shutdown.registrations.push(signal_hook::flag::register_usize(signal, shutdown.signal.clone(), signal as usize)?);
        }
        Ok(shutdown)
    }

    pub fn signal(&self) -> Option<i32> {
        match self.signal.load(Ordering::SeqCst) { 0 => None, signal => Some(signal as i32) }
    }
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) { signal_hook::low_level::unregister(registration); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_signal_reaches_shutdown_owner() {
        let shutdown = Shutdown::new().unwrap();
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
        assert_eq!(shutdown.signal(), Some(signal_hook::consts::SIGTERM));
    }
}
