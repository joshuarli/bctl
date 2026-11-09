use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

const WORKER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gpu2DState {
    pub rutabaga: Vec<u8>,
    pub resources: Vec<Gpu2DResource>,
    pub scanouts: Vec<Option<Gpu2DScanout>>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gpu2DResource {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub format: Option<u32>,
    pub size: u64,
    pub shmem_offset: Option<u64>,
    pub rutabaga_external_mapping: bool,
    pub scanout_mask: u32,
    pub backings: Vec<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Gpu2DScanout {
    pub resource_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

#[derive(Default)]
struct WorkerState {
    snapshot_requested: bool,
    snapshot_reply: Option<Result<Gpu2DState, String>>,
    restore_requested: Option<Gpu2DState>,
    restore_reply: Option<Result<(), String>>,
    restore_pending: bool,
}

pub struct GpuWorkerSync {
    state: Mutex<WorkerState>,
    changed: Condvar,
}

impl GpuWorkerSync {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(WorkerState::default()),
            changed: Condvar::new(),
        }
    }

    fn wait<'a>(
        &self,
        guard: MutexGuard<'a, WorkerState>,
        done: impl Fn(&WorkerState) -> bool,
    ) -> Result<MutexGuard<'a, WorkerState>, String> {
        let (guard, timeout) = self
            .changed
            .wait_timeout_while(guard, WORKER_TIMEOUT, |state| !done(state))
            .map_err(|_| "gpu worker state is poisoned".to_string())?;
        if timeout.timed_out() {
            return Err("gpu worker did not answer in time".to_string());
        }
        Ok(guard)
    }

    pub fn request_snapshot(&self) -> Result<Gpu2DState, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "gpu worker state is poisoned".to_string())?;
        state.snapshot_requested = true;
        state.snapshot_reply = None;
        self.changed.notify_all();
        let mut state = self.wait(state, |state| state.snapshot_reply.is_some())?;
        state
            .snapshot_reply
            .take()
            .expect("waited for the snapshot reply")
    }

    pub fn take_snapshot_request(&self) -> bool {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.snapshot_requested))
            .unwrap_or(false)
    }

    pub fn complete_snapshot(&self, reply: Result<Gpu2DState, String>) {
        if let Ok(mut state) = self.state.lock() {
            state.snapshot_requested = false;
            state.snapshot_reply = Some(reply);
            self.changed.notify_all();
        }
    }

    pub fn request_restore(&self, restore: Gpu2DState) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "gpu worker state is poisoned".to_string())?;
        state.restore_requested = Some(restore);
        state.restore_reply = None;
        self.changed.notify_all();
        let mut state = self.wait(state, |state| state.restore_reply.is_some())?;
        state
            .restore_reply
            .take()
            .expect("waited for the restore reply")
    }

    pub fn take_restore_request(&self) -> Option<Gpu2DState> {
        self.state
            .lock()
            .ok()
            .and_then(|mut state| state.restore_requested.take())
    }

    pub fn complete_restore(&self, reply: Result<(), String>) {
        if let Ok(mut state) = self.state.lock() {
            state.restore_reply = Some(reply);
            self.changed.notify_all();
        }
    }

    pub fn expect_restore(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.restore_pending = true;
        }
    }

    pub fn wait_restore(&self) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "gpu worker state is poisoned".to_string())?;
        if !state.restore_pending {
            return Ok(());
        }
        let mut state = self.wait(state, |state| state.restore_reply.is_some())?;
        state.restore_pending = false;
        state
            .restore_reply
            .take()
            .expect("waited for the restore reply")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sample_state() -> Gpu2DState {
        Gpu2DState {
            rutabaga: vec![1, 2, 3, 4],
            resources: vec![Gpu2DResource {
                id: 7,
                width: 4,
                height: 3,
                format: Some(1),
                size: 48,
                shmem_offset: None,
                rutabaga_external_mapping: false,
                scanout_mask: 1,
                backings: vec![(0x1000, 48)],
            }],
            scanouts: vec![Some(Gpu2DScanout {
                resource_id: 7,
                width: 4,
                height: 3,
                format: 1,
            })],
        }
    }

    #[test]
    fn gpu_2d_state_roundtrips_through_device_snapshot_json() {
        let state = sample_state();
        let bytes = serde_json::to_vec(&state).unwrap();
        let restored: Gpu2DState = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn worker_sync_answers_a_snapshot_request() {
        let sync = Arc::new(GpuWorkerSync::new());
        let worker = sync.clone();
        let thread = std::thread::spawn(move || {
            while !worker.take_snapshot_request() {
                std::thread::yield_now();
            }
            worker.complete_snapshot(Ok(sample_state()));
        });
        assert_eq!(sync.request_snapshot().unwrap(), sample_state());
        thread.join().unwrap();
    }

    #[test]
    fn worker_sync_reports_a_failed_restore() {
        let sync = GpuWorkerSync::new();
        sync.expect_restore();
        sync.complete_restore(Err("no display".to_string()));
        assert_eq!(sync.wait_restore(), Err("no display".to_string()));
    }
}
