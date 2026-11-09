//! Host memory pressure guard: what happens when branch children, having
//! diverged from their source, fill the host's RAM.
//!
//! Branch admission (`agent::fork::admit_branch_memory`) keeps a reserve free
//! at fork time, but children keep writing afterwards and the per-VM cgroup
//! ceilings add up to far more than the host (copy-on-write density is the
//! point). Without this guard the kernel's global OOM killer picks the victim
//! by size, which is the source — and every child dies with it. The guard
//! watches `/proc/pressure/memory` and `MemAvailable` and, when the host is
//! genuinely short, pauses the newest running branch child: its RAM goes to
//! disk, the host breathes, and the machine stays resumable with a recorded
//! reason (`pause_reason`, `state_reason` in the API). When memory has
//! recovered with margin it resumes those machines one at a time.
//!
//! Sources, held pool slots and machines the user paused are never touched.
//! `SMOLVM_MEMORY_PRESSURE_GUARD=0` turns the guard off; the thresholds are
//! `SMOLVM_MEMORY_PRESSURE_RESERVE_MIB` (default: the branch admission
//! reserve), `SMOLVM_MEMORY_PRESSURE_SOME_PCT` (20) and
//! `SMOLVM_MEMORY_PRESSURE_FULL_PCT` (5).
use crate::api::state::ApiState;
use crate::config::{RecordState, VmRecord};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const TICK: Duration = Duration::from_secs(2);
/// One resume per this long: a resumed child needs time to show its real footprint.
const RESUME_COOLDOWN: Duration = Duration::from_secs(30);

/// One reading of the host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Effective host (or cgroup) memory, MiB.
    pub total_mib: u64,
    /// `MemAvailable` (or cgroup headroom), MiB.
    pub available_mib: u64,
    /// `/proc/pressure/memory` `some avg10`: % of the last 10 s some task waited for memory.
    pub some_avg10: f64,
    /// `full avg10`: % of the last 10 s every task was stalled on memory.
    pub full_avg10: f64,
}

/// When the guard acts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Host memory that should stay free (MiB); the guard acts below it.
    pub reserve_mib: u64,
    /// `some avg10` at or above which a host below the reserve counts as under pressure.
    pub some_avg10_pct: f64,
    /// `full avg10` at or above which a host below the reserve counts as under pressure.
    pub full_avg10_pct: f64,
}

/// What the guard knows about one machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Machine name.
    pub name: String,
    /// Creation time (seconds since the epoch): newest children are paused first.
    pub created_at: u64,
    /// Configured guest RAM, MiB: what a resume must fit into the margin.
    pub mem_mib: u64,
    /// A running branch child that is not itself a branch source.
    pub running_clone: bool,
    /// Paused by this guard (a recorded `pause_reason`), so eligible to resume.
    pub paused_by_guard: bool,
    /// A held pool slot: parked on purpose, never a victim.
    pub held: bool,
}

/// The guard's action for one tick.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Pause this branch child to relieve the host.
    Pause(String),
    /// Resume this child the guard paused earlier.
    Resume(String),
    /// Nothing this tick.
    Hold,
}

/// `some avg10` and `full avg10` from `/proc/pressure/memory`.
pub fn parse_pressure(text: &str) -> Option<(f64, f64)> {
    let mut some = None;
    let mut full = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let kind = parts.next()?;
        let avg10 = parts
            .find_map(|p| p.strip_prefix("avg10="))
            .and_then(|v| v.parse::<f64>().ok())?;
        match kind {
            "some" => some = Some(avg10),
            "full" => full = Some(avg10),
            _ => {}
        }
    }
    Some((some?, full.unwrap_or(0.0)))
}

/// Whether the host is short of memory right now: below half the reserve
/// whatever the kernel says, or below the reserve while tasks are already
/// stalling on memory.
pub fn under_pressure(sample: &Sample, t: &Thresholds) -> bool {
    sample.available_mib < t.reserve_mib / 2
        || (sample.available_mib < t.reserve_mib
            && (sample.some_avg10 >= t.some_avg10_pct || sample.full_avg10 >= t.full_avg10_pct))
}

/// The one action for this tick. Under pressure: pause the newest running
/// branch child. With margin (twice the reserve free plus the child's RAM):
/// resume the guard's oldest victim. Otherwise nothing.
pub fn decide(sample: &Sample, t: &Thresholds, candidates: &[Candidate]) -> Decision {
    if under_pressure(sample, t) {
        return candidates
            .iter()
            .filter(|c| c.running_clone && !c.held)
            .max_by_key(|c| (c.created_at, c.name.clone()))
            .map(|c| Decision::Pause(c.name.clone()))
            .unwrap_or(Decision::Hold);
    }
    let margin = sample
        .available_mib
        .saturating_sub(t.reserve_mib.saturating_mul(2));
    if margin == 0 {
        return Decision::Hold;
    }
    candidates
        .iter()
        .filter(|c| c.paused_by_guard && c.mem_mib <= margin)
        .min_by_key(|c| (c.created_at, c.name.clone()))
        .map(|c| Decision::Resume(c.name.clone()))
        .unwrap_or(Decision::Hold)
}

/// What the guard may do with one machine, from its record.
pub fn candidate(record: &VmRecord) -> Candidate {
    let running_clone = record.golden.is_some()
        && !record.forkable
        && record.state == RecordState::Running
        && record.is_process_alive();
    Candidate {
        name: record.name.clone(),
        created_at: record.created_at,
        mem_mib: u64::from(record.mem),
        running_clone,
        paused_by_guard: record.pause_reason.is_some() && record.state == RecordState::Paused,
        held: record.forkpoint_held,
    }
}

/// The host right now: memory and pressure.
#[cfg(target_os = "linux")]
pub fn read_sample() -> Option<Sample> {
    const MIB: u64 = 1024 * 1024;
    let memory = crate::process::host_memory_stats()?;
    let (some_avg10, full_avg10) = std::fs::read_to_string("/proc/pressure/memory")
        .ok()
        .and_then(|t| parse_pressure(&t))
        .unwrap_or((0.0, 0.0));
    Some(Sample {
        total_mib: memory.total_bytes / MIB,
        available_mib: memory.available_bytes / MIB,
        some_avg10,
        full_avg10,
    })
}

/// The host right now; no pressure file off Linux, so no guard.
#[cfg(not(target_os = "linux"))]
pub fn read_sample() -> Option<Sample> {
    None
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The thresholds for a host with `total_mib` of memory, or None when the
/// guard is off (explicitly, or because branch admission is off).
pub fn thresholds(total_mib: u64) -> Option<Thresholds> {
    if std::env::var("SMOLVM_MEMORY_PRESSURE_GUARD").as_deref() == Ok("0") {
        return None;
    }
    let reserve_mib = match std::env::var("SMOLVM_MEMORY_PRESSURE_RESERVE_MIB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(v) => v,
        None => {
            crate::agent::fork::branch_admission_params(total_mib)
                .ok()
                .flatten()?
                .reserve_mib
        }
    };
    Some(Thresholds {
        reserve_mib,
        some_avg10_pct: env_f64("SMOLVM_MEMORY_PRESSURE_SOME_PCT", 20.0),
        full_avg10_pct: env_f64("SMOLVM_MEMORY_PRESSURE_FULL_PCT", 5.0),
    })
}

/// The guard loop; returns on shutdown.
pub async fn run(state: Arc<ApiState>, mut shutdown_rx: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_resume: Option<Instant> = None;
    let mut announced = false;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let Some(sample) = read_sample() else { continue };
                let Some(t) = thresholds(sample.total_mib) else { continue };
                if !announced {
                    tracing::info!(reserve_mib = t.reserve_mib, some_pct = t.some_avg10_pct, full_pct = t.full_avg10_pct, "memory pressure guard active");
                    announced = true;
                }
                let candidates: Vec<Candidate> = state
                    .list_machine_names()
                    .into_iter()
                    .filter_map(|n| state.db().get_vm(&n).ok().flatten())
                    .map(|r| candidate(&r))
                    .collect();
                match decide(&sample, &t, &candidates) {
                    Decision::Pause(name) => pause(&state, &name, &sample, &t).await,
                    Decision::Resume(name) => {
                        if last_resume.is_none_or(|at| at.elapsed() >= RESUME_COOLDOWN)
                            && resume(&state, &name).await
                        {
                            last_resume = Some(Instant::now());
                        }
                    }
                    Decision::Hold => {}
                }
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }
}

async fn pause(state: &Arc<ApiState>, name: &str, sample: &Sample, t: &Thresholds) {
    let reason = format!(
        "host memory pressure: {} MiB available of {} (reserve {} MiB), some {:.0}% full {:.0}%",
        sample.available_mib, sample.total_mib, t.reserve_mib, sample.some_avg10, sample.full_avg10
    );
    tracing::warn!(machine = %name, %reason, "pausing the newest branch child to relieve the host");
    let _guard = state.lifecycle_lock(name).lock_owned().await;
    let db = state.db().clone();
    let (name2, reason2) = (name.to_string(), reason.clone());
    let outcome = tokio::task::spawn_blocking(move || {
        let runtime = crate::embedded::EmbeddedRuntime::with_db(db.clone());
        let operation = format!(
            "host-memory-pressure-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );
        runtime.pause_machine_with_operation(&name2, &operation)?;
        db.update_vm(&name2, |record| record.pause_reason = Some(reason2))?;
        Ok::<(), crate::Error>(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => {
            metrics::counter!("smolvm_memory_pressure_pauses_total").increment(1);
            tracing::warn!(machine = %name, "paused for host memory pressure; resumes when memory recovers");
        }
        Ok(Err(e)) => tracing::warn!(machine = %name, error = %e, "memory pressure pause failed"),
        Err(e) => tracing::warn!(machine = %name, error = %e, "memory pressure pause task failed"),
    }
}

async fn resume(state: &Arc<ApiState>, name: &str) -> bool {
    tracing::info!(machine = %name, "host memory recovered; resuming a child the guard paused");
    let _guard = state.lifecycle_lock(name).lock_owned().await;
    let db = state.db().clone();
    let name2 = name.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        let runtime = crate::embedded::EmbeddedRuntime::with_db(db.clone());
        // detached: the restored VMM must outlive this runtime handle
        runtime.resume_machine_detached(&name2)?;
        db.update_vm(&name2, |record| record.pause_reason = None)?;
        Ok::<(), crate::Error>(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => {
            metrics::counter!("smolvm_memory_pressure_resumes_total").increment(1);
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(machine = %name, error = %e, "memory pressure resume failed");
            false
        }
        Err(e) => {
            tracing::warn!(machine = %name, error = %e, "memory pressure resume task failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Thresholds {
        Thresholds {
            reserve_mib: 2048,
            some_avg10_pct: 20.0,
            full_avg10_pct: 5.0,
        }
    }
    fn s(available_mib: u64, some: f64, full: f64) -> Sample {
        Sample {
            total_mib: 65536,
            available_mib,
            some_avg10: some,
            full_avg10: full,
        }
    }
    fn c(name: &str, created_at: u64, mem: u64) -> Candidate {
        Candidate {
            name: name.into(),
            created_at,
            mem_mib: mem,
            running_clone: true,
            paused_by_guard: false,
            held: false,
        }
    }

    #[test]
    fn parses_the_kernel_pressure_file() {
        let text = "some avg10=12.50 avg60=3.10 avg300=0.80 total=3836020838\nfull avg10=4.00 avg60=1.00 avg300=0.20 total=3391985927\n";
        assert_eq!(parse_pressure(text), Some((12.5, 4.0)));
        assert_eq!(
            parse_pressure("some avg10=0.00 avg60=0.00 avg300=0.00 total=1\n"),
            Some((0.0, 0.0))
        );
        assert_eq!(parse_pressure("garbage"), None);
    }

    #[test]
    fn pressure_needs_low_memory_and_stalls_or_very_low_memory() {
        assert!(
            !under_pressure(&s(10_000, 50.0, 10.0), &t()),
            "plenty of memory: stalls alone are not our problem"
        );
        assert!(
            !under_pressure(&s(1_500, 2.0, 0.0), &t()),
            "below the reserve but nobody is stalling yet"
        );
        assert!(
            under_pressure(&s(1_500, 25.0, 0.0), &t()),
            "below the reserve and tasks wait for memory"
        );
        assert!(under_pressure(&s(1_500, 0.0, 6.0), &t()));
        assert!(
            under_pressure(&s(900, 0.0, 0.0), &t()),
            "below half the reserve: act before the kernel does"
        );
    }

    #[test]
    fn the_newest_running_child_is_paused_first_and_sources_and_held_slots_never() {
        let mut source = c("golden", 1, 8192);
        source.running_clone = false;
        let mut held = c("slot-3", 300, 1024);
        held.held = true;
        let cands = vec![source, c("w-0", 100, 1024), c("w-1", 200, 1024), held];
        assert_eq!(
            decide(&s(900, 0.0, 0.0), &t(), &cands),
            Decision::Pause("w-1".into())
        );
        let only_source = vec![cands[0].clone()];
        assert_eq!(
            decide(&s(900, 0.0, 0.0), &t(), &only_source),
            Decision::Hold,
            "nothing we may pause"
        );
    }

    #[test]
    fn resume_needs_margin_and_takes_the_oldest_victim_that_fits() {
        let mut p0 = c("w-0", 100, 1024);
        p0.running_clone = false;
        p0.paused_by_guard = true;
        let mut p1 = c("w-1", 200, 4096);
        p1.running_clone = false;
        p1.paused_by_guard = true;
        let mut user_paused = c("w-9", 50, 512);
        user_paused.running_clone = false;
        let cands = vec![p1.clone(), p0.clone(), user_paused];
        assert_eq!(
            decide(&s(3_000, 0.0, 0.0), &t(), &cands),
            Decision::Hold,
            "free but no margin over twice the reserve"
        );
        assert_eq!(
            decide(&s(5_200, 0.0, 0.0), &t(), &cands),
            Decision::Resume("w-0".into()),
            "oldest victim that fits the margin"
        );
        assert_eq!(
            decide(&s(9_000, 0.0, 0.0), &t(), &cands),
            Decision::Resume("w-0".into())
        );
        assert_eq!(
            decide(&s(9_000, 0.0, 0.0), &t(), &[p1]),
            Decision::Resume("w-1".into())
        );
    }

    #[test]
    fn nothing_to_do_in_the_middle() {
        assert_eq!(
            decide(&s(3_000, 0.0, 0.0), &t(), &[c("w-0", 1, 512)]),
            Decision::Hold
        );
    }
}
