use std::collections::BTreeMap;

use crate::names::{MachineName, SessionName};
use crate::state::{AttemptRecord, MachineObservation, SessionRecord, SessionState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The session's current-generation VM is running: keep ownership and carry on from its recorded state.
    Adopt { session: SessionName, machine: MachineName },
    /// The session's VM is gone; record the session as `state` with `reason`.
    Settle { session: SessionName, state: SessionState, reason: Option<String> },
    /// A bctl VM for a known session but not its current generation; it must be stopped, never adopted.
    StopStale { machine: MachineName },
    /// A live attempt whose owner vanished; its effects are unknown, so it ends interrupted and is not retried.
    Interrupt { attempt: i64 },
}

/// Decides how to restore ownership after the daemon starts. Machines bctl did not name, and bctl machines for
/// sessions this store does not know, are left alone: another state directory may own them.
pub fn reconcile(sessions: &[SessionRecord], attempts: &[AttemptRecord], machines: &[MachineObservation]) -> Vec<Action> {
    let mut running: BTreeMap<SessionName, Vec<MachineName>> = BTreeMap::new();
    for machine in machines.iter().filter(|m| m.running) {
        if let Some(name) = MachineName::parse(&machine.name) {
            running.entry(name.session.clone()).or_default().push(name);
        }
    }

    let mut actions = Vec::new();
    for session in sessions {
        let machines = running.remove(&session.name).unwrap_or_default();
        let (current, stale): (Vec<_>, Vec<_>) =
            machines.into_iter().partition(|m| session.state.holds_machine() && m.generation == session.generation);
        actions.extend(stale.into_iter().map(|machine| Action::StopStale { machine }));
        match (session.state, current.into_iter().next()) {
            (SessionState::Stopping, Some(machine)) | (SessionState::Starting | SessionState::Ready, Some(machine)) => {
                actions.push(Action::Adopt { session: session.name.clone(), machine });
            }
            (SessionState::Stopping, None) => {
                actions.push(Action::Settle { session: session.name.clone(), state: SessionState::Stopped, reason: None });
            }
            (SessionState::Starting | SessionState::Ready, None) => actions.push(Action::Settle {
                session: session.name.clone(),
                state: SessionState::Failed,
                reason: Some(format!("machine for generation {} vanished while bctl was not running", session.generation)),
            }),
            (SessionState::Stopped | SessionState::Failed, _) => {}
        }
    }
    actions.extend(attempts.iter().filter(|a| a.state.is_live()).map(|a| Action::Interrupt { attempt: a.id }));
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::{Generation, ProfileName};
    use crate::state::AttemptState;

    fn session(name: &str, state: SessionState, generation: u64) -> SessionRecord {
        SessionRecord {
            name: SessionName::new(name).unwrap(),
            profile: ProfileName::new("personal").unwrap(),
            image: "ref-alpine-chromium".into(),
            state,
            generation: Generation(generation),
            failure: None,
        }
    }

    fn machine(name: &str, running: bool) -> MachineObservation {
        MachineObservation { name: name.into(), running }
    }

    #[test]
    fn a_running_current_generation_machine_is_adopted() {
        let actions = reconcile(&[session("research", SessionState::Ready, 3)], &[], &[machine("bctl-research-3", true)]);
        assert_eq!(actions, [Action::Adopt { session: SessionName::new("research").unwrap(), machine: MachineName::parse("bctl-research-3").unwrap() }]);
    }

    #[test]
    fn a_machine_from_another_generation_is_stopped_never_adopted() {
        let actions = reconcile(&[session("research", SessionState::Ready, 4)], &[], &[machine("bctl-research-3", true)]);
        assert_eq!(
            actions,
            [
                Action::StopStale { machine: MachineName::parse("bctl-research-3").unwrap() },
                Action::Settle {
                    session: SessionName::new("research").unwrap(),
                    state: SessionState::Failed,
                    reason: Some("machine for generation 4 vanished while bctl was not running".into()),
                },
            ]
        );
    }

    #[test]
    fn a_stopped_session_never_adopts_its_old_machine() {
        let actions = reconcile(&[session("research", SessionState::Stopped, 3)], &[], &[machine("bctl-research-3", true)]);
        assert_eq!(actions, [Action::StopStale { machine: MachineName::parse("bctl-research-3").unwrap() }]);
    }

    #[test]
    fn a_stop_that_finished_while_bctl_was_down_settles_as_stopped() {
        let actions = reconcile(&[session("research", SessionState::Stopping, 2)], &[], &[machine("bctl-research-2", false)]);
        assert_eq!(actions, [Action::Settle { session: SessionName::new("research").unwrap(), state: SessionState::Stopped, reason: None }]);
    }

    #[test]
    fn foreign_and_unknown_machines_are_left_alone() {
        let actions = reconcile(&[], &[], &[machine("vm-0a4a7c5c", true), machine("bctl-elsewhere-1", true)]);
        assert_eq!(actions, []);
    }

    #[test]
    fn live_attempts_are_interrupted_and_finished_ones_untouched() {
        let attempts = [
            AttemptRecord { id: 1, run: 1, state: AttemptState::Running },
            AttemptRecord { id: 2, run: 1, state: AttemptState::NeedsInteraction },
            AttemptRecord { id: 3, run: 2, state: AttemptState::Succeeded },
            AttemptRecord { id: 4, run: 3, state: AttemptState::Starting },
        ];
        let actions = reconcile(&[], &attempts, &[]);
        assert_eq!(actions, [Action::Interrupt { attempt: 1 }, Action::Interrupt { attempt: 2 }, Action::Interrupt { attempt: 4 }]);
    }
}
