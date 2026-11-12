use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use bctl_automation::Interrupt;
use bctl_core::SessionName;

use crate::daemon::Daemon;

/// A dashboard viewer, named by its dashboard session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Viewer(String);

impl Viewer {
    pub(crate) fn new(dashboard_session: &str) -> Viewer {
        Viewer(dashboard_session.to_string())
    }
}

/// An external CDP controller, named by the capability token its sockets were opened with. Every socket opened
/// through one token belongs to one controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Controller(String);

impl Controller {
    pub(crate) fn new(token: &str) -> Controller {
        Controller(token.to_string())
    }
}

/// Who acts on a session's browser. A person's input reaches it only in `Human`, which is entered only once
/// every native command begun under `Automation` has ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Authority {
    Automation,
    /// The viewer asked for control and native commands begun under automation are still ending.
    Yielding(Viewer),
    /// The viewer holds the RFB input lease.
    Human(Viewer),
}

impl Authority {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Authority::Automation => "automation",
            Authority::Yielding(_) => "yielding",
            Authority::Human(_) => "human",
        }
    }

    pub(crate) fn viewer(&self) -> Option<&Viewer> {
        match self {
            Authority::Automation => None,
            Authority::Yielding(viewer) | Authority::Human(viewer) => Some(viewer),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    Reads,
    /// Can change the browser's pages: refused outside `Automation`.
    Mutates,
}

/// Another viewer holds control, or is taking it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Contested;

/// A command that would change the pages, refused because automation does not hold control.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) Authority);

/// A takeover refused: another viewer holds or is taking control, or an external controller is attached.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TakeRefused {
    Contested,
    ControllerAttached { connections: usize },
}

/// A CDP attachment refused: a person holds or is taking control, or another controller is attached.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AttachRefused {
    Person,
    OtherController,
}

/// A native command's standing with one session's control.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ticket {
    /// Begun under `Automation`, so a takeover awaits its end.
    awaited: bool,
    takeovers_seen: u64,
}

/// An attached external controller with its open CDP sockets.
#[derive(Debug)]
struct Held {
    controller: Controller,
    sockets: usize,
    /// The run it stood aside for has resumed: its sockets are being closed and it no longer holds the session.
    evicted: bool,
}

/// One session's authority, the native commands still settling, which a takeover waits for, and the external
/// controllers attached with their open CDP sockets. Normally at most one controller is attached. While the
/// session's run waits for a person, the run's own controller stands aside and one other may attach beside it.
#[derive(Debug)]
pub(crate) struct Control {
    authority: Authority,
    settling: usize,
    takeovers: u64,
    attached: Vec<Held>,
    aside: Option<Controller>,
}

impl Default for Control {
    fn default() -> Control {
        Control { authority: Authority::Automation, settling: 0, takeovers: 0, attached: Vec::new(), aside: None }
    }
}

impl Control {
    pub(crate) fn authority(&self) -> &Authority {
        &self.authority
    }

    /// Native commands begun under automation that have not ended yet.
    pub(crate) fn settling(&self) -> usize {
        self.settling
    }

    /// The open CDP sockets that hold the session, or 0: not those of a controller standing aside for a person,
    /// nor of one being closed.
    pub(crate) fn connections(&self) -> usize {
        self.attached.iter().filter(|held| self.holds(held)).map(|held| held.sockets).sum()
    }

    fn holds(&self, held: &Held) -> bool {
        !held.evicted && self.aside.as_ref() != Some(&held.controller)
    }

    /// Opens a CDP socket for `controller`. The first makes it the session's one controller; more from the same
    /// controller share that standing. Refused while a person holds or takes control, or another is attached.
    pub(crate) fn attach(&mut self, controller: &Controller) -> Result<(), AttachRefused> {
        self.admits(controller)?;
        match self.attached.iter_mut().find(|held| held.controller == *controller) {
            Some(held) => held.sockets += 1,
            None => self.attached.push(Held { controller: controller.clone(), sockets: 1, evicted: false }),
        }
        Ok(())
    }

    /// Whether `controller` could attach now. The controller of a run that waits for a person is always
    /// admitted, and does not keep another from attaching.
    pub(crate) fn admits(&self, controller: &Controller) -> Result<(), AttachRefused> {
        if self.authority != Authority::Automation {
            return Err(AttachRefused::Person);
        }
        if self.aside.as_ref() == Some(controller) {
            return Ok(());
        }
        let refused = self.attached.iter().any(|held| if held.controller == *controller { held.evicted } else { self.holds(held) });
        if refused { Err(AttachRefused::OtherController) } else { Ok(()) }
    }

    /// One of `controller`'s sockets closed; its last releases the session. A socket of a controller that no
    /// longer holds it, such as one closing after the session stopped, changes nothing.
    pub(crate) fn detach(&mut self, controller: &Controller) {
        if let Some(index) = self.attached.iter().position(|held| held.controller == *controller) {
            self.attached[index].sockets -= 1;
            if self.attached[index].sockets == 0 {
                self.attached.remove(index);
            }
        }
    }

    /// The session's run waits for a person, and `controller` is the run's. Until the run resumes or ends, that
    /// controller no longer excludes another one, such as the operator's, nor a takeover.
    pub(crate) fn stand_aside(&mut self, controller: &Controller) {
        self.aside = Some(controller.clone());
    }

    /// The run resumes: the controllers that attached beside it are evicted. They stop holding the session at
    /// once and their sockets are closed. Does nothing unless a run was waiting.
    pub(crate) fn resume_run(&mut self) {
        let Some(run) = self.aside.take() else { return };
        for held in self.attached.iter_mut().filter(|held| held.controller != run) {
            held.evicted = true;
        }
    }

    /// The run ended while it waited: whoever is attached keeps the session, and the run's controller no longer
    /// stands aside.
    pub(crate) fn run_ended(&mut self) {
        self.aside = None;
    }

    /// Whether `controller` was evicted and its sockets should be closed.
    pub(crate) fn evicted(&self, controller: &Controller) -> bool {
        self.attached.iter().any(|held| held.controller == *controller && held.evicted)
    }

    /// Moves to `Yielding`, or straight to `Human` if no native command is settling. Asking again
    /// changes nothing. Refused while an external controller is attached: a person's input and a controller's
    /// commands already on its sockets would interleave.
    pub(crate) fn take(&mut self, viewer: &Viewer) -> Result<(), TakeRefused> {
        if self.authority == Authority::Automation && self.connections() > 0 {
            return Err(TakeRefused::ControllerAttached { connections: self.connections() });
        }
        match &self.authority {
            Authority::Automation => {
                self.takeovers += 1;
                self.authority = if self.settling == 0 { Authority::Human(viewer.clone()) } else { Authority::Yielding(viewer.clone()) };
                Ok(())
            }
            held if held.viewer() == Some(viewer) => Ok(()),
            _ => Err(TakeRefused::Contested),
        }
    }

    /// Hands control back to automation, from `Human` or from a `Yielding` that has not finished.
    pub(crate) fn resume(&mut self, viewer: &Viewer) -> Result<(), Contested> {
        match &self.authority {
            Authority::Automation => Ok(()),
            held if held.viewer() == Some(viewer) => {
                self.authority = Authority::Automation;
                Ok(())
            }
            _ => Err(Contested),
        }
    }

    /// The viewer went away: control it held or was taking goes back to automation.
    pub(crate) fn abandon(&mut self, viewer: &Viewer) {
        if self.authority.viewer() == Some(viewer) {
            self.authority = Authority::Automation;
        }
    }

    /// The session stopped: whoever held control, automation holds it now, and no controller is attached.
    pub(crate) fn reset(&mut self) {
        self.authority = Authority::Automation;
        self.attached.clear();
        self.aside = None;
    }

    pub(crate) fn begin(&mut self, effect: Effect) -> Result<Ticket, Refused> {
        match (&self.authority, effect) {
            (Authority::Automation, _) => {
                self.settling += 1;
                Ok(Ticket { awaited: true, takeovers_seen: self.takeovers })
            }
            (_, Effect::Reads) => Ok(Ticket { awaited: false, takeovers_seen: self.takeovers }),
            (held, Effect::Mutates) => Err(Refused(held.clone())),
        }
    }

    /// The command ended. The last one a takeover waited for lets the person in.
    pub(crate) fn end(&mut self, ticket: Ticket) {
        if !ticket.awaited {
            return;
        }
        self.settling -= 1;
        if self.settling == 0
            && let Authority::Yielding(viewer) = &self.authority
        {
            self.authority = Authority::Human(viewer.clone());
        }
    }

    /// Whether a person has asked for control since the command began. A command stops when it is, and asks
    /// once more right before it sends input: `take` counts the takeover in the same critical section that
    /// leaves `Automation`, so a command told no here began its input under automation, and `Human` waits for
    /// it to end.
    pub(crate) fn interrupts(&self, ticket: &Ticket) -> bool {
        self.takeovers > ticket.takeovers_seen
    }
}

/// Every session's control, owned by the daemon. A session no one has taken is under automation.
#[derive(Default)]
pub(crate) struct Authorities(Mutex<HashMap<SessionName, Control>>);

impl Authorities {
    pub(crate) fn of<T>(&self, session: &SessionName, f: impl FnOnce(&mut Control) -> T) -> T {
        let mut sessions: MutexGuard<'_, _> = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(sessions.entry(session.clone()).or_default())
    }

    pub(crate) fn begin(&self, session: &SessionName, effect: Effect) -> Result<InFlight<'_>, Refused> {
        let ticket = self.of(session, |control| control.begin(effect))?;
        Ok(InFlight { authorities: self, session: session.clone(), ticket: Some(ticket) })
    }
}

/// One open CDP socket of an attached controller. Dropping it closes the socket's share of the attachment.
pub(crate) struct Attachment {
    daemon: Arc<Daemon>,
    session: SessionName,
    controller: Controller,
}

impl Attachment {
    pub(crate) fn open(daemon: &Arc<Daemon>, session: &SessionName, controller: Controller) -> Result<Attachment, AttachRefused> {
        daemon.authority.of(session, |control| control.attach(&controller))?;
        Ok(Attachment { daemon: Arc::clone(daemon), session: session.clone(), controller })
    }
}

/// What a socket's watcher asks about its controller.
pub(crate) struct Standing {
    daemon: Arc<Daemon>,
    session: SessionName,
    controller: Controller,
}

impl Standing {
    /// Whether the controller was evicted, so the socket should close.
    pub(crate) fn evicted(&self) -> bool {
        self.daemon.authority.of(&self.session, |control| control.evicted(&self.controller))
    }
}

impl Attachment {
    pub(crate) fn standing(&self) -> Standing {
        Standing { daemon: Arc::clone(&self.daemon), session: self.session.clone(), controller: self.controller.clone() }
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.daemon.authority.of(&self.session, |control| control.detach(&self.controller));
    }
}

/// A native command in flight. Dropping it ends the command.
pub(crate) struct InFlight<'a> {
    authorities: &'a Authorities,
    session: SessionName,
    ticket: Option<Ticket>,
}

impl Interrupt for InFlight<'_> {
    fn interrupted(&self) -> bool {
        let ticket = self.ticket.as_ref().expect("a command in flight holds its ticket");
        self.authorities.of(&self.session, |control| control.interrupts(ticket))
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.authorities.of(&self.session, |control| control.end(ticket));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewers() -> (Viewer, Viewer) {
        (Viewer::new("a"), Viewer::new("b"))
    }

    #[test]
    fn automation_runs_every_command_and_a_take_with_nothing_in_flight_hands_over_at_once() {
        let (a, _) = viewers();
        let mut control = Control::default();
        let read = control.begin(Effect::Reads).unwrap();
        let write = control.begin(Effect::Mutates).unwrap();
        assert_eq!(control.settling(), 2);
        control.end(read);
        control.end(write);
        assert_eq!(control.resume(&a), Ok(()), "resuming under automation changes nothing");
        assert_eq!(control.authority(), &Authority::Automation);
        assert_eq!(control.take(&a), Ok(()));
        assert_eq!(control.authority(), &Authority::Human(a));
    }

    #[test]
    fn a_take_yields_until_every_command_begun_under_automation_ends_then_hands_over() {
        let (a, _) = viewers();
        let mut control = Control::default();
        let first = control.begin(Effect::Mutates).unwrap();
        let second = control.begin(Effect::Reads).unwrap();
        assert!(!control.interrupts(&first));
        control.take(&a).unwrap();
        assert_eq!(control.authority(), &Authority::Yielding(a.clone()));
        assert!(control.interrupts(&first) && control.interrupts(&second), "commands in flight learn of the takeover");
        control.end(first);
        assert_eq!(control.authority(), &Authority::Yielding(a.clone()), "one command is still settling");
        control.end(second);
        assert_eq!(control.authority(), &Authority::Human(a));
        assert_eq!(control.settling(), 0);
    }

    #[test]
    fn outside_automation_reads_run_unawaited_and_changes_are_refused() {
        fn check(control: &mut Control, held: &Authority) {
            assert_eq!(control.authority(), held);
            assert_eq!(control.begin(Effect::Mutates), Err(Refused(held.clone())));
            let read = control.begin(Effect::Reads).unwrap();
            assert!(!control.interrupts(&read), "a read begun after the takeover is not stopped by it");
            control.end(read);
            assert_eq!(control.authority(), held, "a read begun outside automation never decides the takeover");
        }
        let (a, _) = viewers();
        let mut control = Control::default();
        let pending = control.begin(Effect::Reads).unwrap();
        control.take(&a).unwrap();
        check(&mut control, &Authority::Yielding(a.clone()));
        control.end(pending);
        check(&mut control, &Authority::Human(a));
    }

    #[test]
    fn only_the_viewer_taking_or_holding_control_can_resume_and_others_are_refused() {
        let (a, b) = viewers();
        let mut control = Control::default();
        let pending = control.begin(Effect::Reads).unwrap();
        control.take(&a).unwrap();
        assert_eq!(control.take(&a), Ok(()), "taking again while yielding changes nothing");
        assert_eq!(control.take(&b), Err(TakeRefused::Contested));
        assert_eq!(control.resume(&b), Err(Contested));
        assert_eq!(control.authority(), &Authority::Yielding(a.clone()));
        assert_eq!(control.resume(&a), Ok(()), "a takeover can be called off while it yields");
        assert_eq!(control.authority(), &Authority::Automation);
        control.end(pending);
        assert_eq!(control.authority(), &Authority::Automation, "a command ending under automation hands nothing over");

        control.take(&a).unwrap();
        assert_eq!(control.authority(), &Authority::Human(a.clone()));
        assert_eq!(control.take(&a), Ok(()));
        assert_eq!(control.take(&b), Err(TakeRefused::Contested));
        assert_eq!(control.resume(&b), Err(Contested));
        assert_eq!(control.authority(), &Authority::Human(a.clone()));
        assert_eq!(control.resume(&a), Ok(()));
        assert_eq!(control.authority(), &Authority::Automation);
        assert_eq!(control.take(&b), Ok(()), "once resumed anyone may take control");
        assert_eq!(control.authority(), &Authority::Human(b));
    }

    #[test]
    fn a_viewer_that_goes_away_gives_control_back_and_a_stopped_session_starts_under_automation() {
        let (a, b) = viewers();
        let mut control = Control::default();
        control.take(&a).unwrap();
        control.abandon(&b);
        assert_eq!(control.authority(), &Authority::Human(a.clone()), "another viewer leaving changes nothing");
        control.abandon(&a);
        assert_eq!(control.authority(), &Authority::Automation);

        let pending = control.begin(Effect::Mutates).unwrap();
        control.take(&a).unwrap();
        control.abandon(&a);
        assert_eq!(control.authority(), &Authority::Automation, "a viewer leaving mid-takeover calls it off");
        control.end(pending);
        assert_eq!(control.authority(), &Authority::Automation);

        control.take(&b).unwrap();
        control.reset();
        assert_eq!(control.authority(), &Authority::Automation);
        assert!(control.begin(Effect::Mutates).is_ok());
    }

    #[test]
    fn a_command_that_saw_a_takeover_stays_interrupted_after_automation_resumes() {
        let (a, _) = viewers();
        let mut control = Control::default();
        let click = control.begin(Effect::Mutates).unwrap();
        control.take(&a).unwrap();
        control.resume(&a).unwrap();
        assert_eq!(control.authority(), &Authority::Automation);
        assert!(control.interrupts(&click), "a command still preparing when a person asked never sends its input");
        control.end(click);
        let next = control.begin(Effect::Mutates).unwrap();
        assert!(!control.interrupts(&next), "a command begun after the resume runs");
    }

    #[test]
    fn a_command_begun_under_a_person_is_interrupted_only_by_a_later_takeover() {
        let (a, b) = viewers();
        let mut control = Control::default();
        control.take(&a).unwrap();
        let read = control.begin(Effect::Reads).unwrap();
        control.resume(&a).unwrap();
        assert!(!control.interrupts(&read));
        control.take(&b).unwrap();
        assert!(control.interrupts(&read));
        control.end(read);
        assert_eq!(control.authority(), &Authority::Human(b));
    }

    #[test]
    fn one_controller_holds_the_session_across_its_sockets_and_others_wait_for_its_last_to_close() {
        let (a, b) = (Controller::new("a"), Controller::new("b"));
        let mut control = Control::default();
        assert_eq!(control.attach(&a), Ok(()));
        assert_eq!(control.attach(&a), Ok(()), "a second socket of the same controller shares its standing");
        assert_eq!(control.connections(), 2);
        assert_eq!(control.attach(&b), Err(AttachRefused::OtherController));
        control.detach(&b);
        assert_eq!(control.connections(), 2, "a refused controller closing changes nothing");
        control.detach(&a);
        assert_eq!(control.attach(&b), Err(AttachRefused::OtherController), "one socket still holds the session");
        control.detach(&a);
        assert_eq!(control.connections(), 0);
        control.detach(&a);
        assert_eq!(control.connections(), 0, "closing again is harmless");
        assert_eq!(control.attach(&b), Ok(()), "once released, another controller may attach");
    }

    #[test]
    fn an_attached_controller_refuses_takeovers_while_native_commands_go_on() {
        let (viewer, _) = viewers();
        let controller = Controller::new("a");
        let mut control = Control::default();
        control.attach(&controller).unwrap();
        control.attach(&controller).unwrap();
        assert_eq!(control.take(&viewer), Err(TakeRefused::ControllerAttached { connections: 2 }));
        assert_eq!(control.authority(), &Authority::Automation);
        let change = control.begin(Effect::Mutates).expect("native commands are automation too");
        control.end(change);
        control.detach(&controller);
        control.detach(&controller);
        assert_eq!(control.take(&viewer), Ok(()), "the takeover goes ahead once the controller has detached");
        assert_eq!(control.authority(), &Authority::Human(viewer));
    }

    #[test]
    fn a_person_holding_or_taking_control_refuses_attachments() {
        let (viewer, _) = viewers();
        let controller = Controller::new("a");
        let mut control = Control::default();
        let pending = control.begin(Effect::Reads).unwrap();
        control.take(&viewer).unwrap();
        assert_eq!(control.attach(&controller), Err(AttachRefused::Person), "refused while yielding");
        control.end(pending);
        assert_eq!(control.attach(&controller), Err(AttachRefused::Person), "refused while a person holds control");
        control.resume(&viewer).unwrap();
        assert_eq!(control.attach(&controller), Ok(()));
    }

    #[test]
    fn a_stopped_session_has_no_controller_and_late_closes_of_old_sockets_leave_a_new_one_alone() {
        let (old, new) = (Controller::new("old"), Controller::new("new"));
        let mut control = Control::default();
        control.attach(&old).unwrap();
        control.reset();
        assert_eq!(control.connections(), 0);
        control.attach(&new).unwrap();
        control.detach(&old);
        assert_eq!(control.connections(), 1, "the old controller's socket closing late does not release the new one");
        assert_eq!(control.take(&Viewer::new("v")), Err(TakeRefused::ControllerAttached { connections: 1 }));
    }

    #[test]
    fn an_operator_attaches_beside_a_run_that_waits_for_a_person_and_is_refused_otherwise() {
        let (run, operator, other) = (Controller::new("run"), Controller::new("operator"), Controller::new("other"));
        let mut control = Control::default();
        control.attach(&run).unwrap();
        assert_eq!(control.admits(&operator), Err(AttachRefused::OtherController), "refused while the run is running");
        assert_eq!(control.attach(&operator), Err(AttachRefused::OtherController));

        control.stand_aside(&run);
        assert_eq!(control.attach(&operator), Ok(()), "admitted while the run waits for a person");
        assert_eq!(control.attach(&operator), Ok(()), "a second socket of the operator shares its standing");
        assert_eq!(control.attach(&run), Ok(()), "the run's own controller is not shut out by the operator");
        assert_eq!(control.attach(&other), Err(AttachRefused::OtherController), "the exception admits one operator, not several");
        assert_eq!(control.connections(), 2, "the run's sockets do not count");
    }

    #[test]
    fn resuming_the_run_evicts_the_operator_and_restores_the_one_controller_rule() {
        let (run, operator, other) = (Controller::new("run"), Controller::new("operator"), Controller::new("other"));
        let mut control = Control::default();
        control.attach(&run).unwrap();
        control.stand_aside(&run);
        control.attach(&operator).unwrap();
        control.attach(&operator).unwrap();

        control.resume_run();
        assert!(control.evicted(&operator) && !control.evicted(&run));
        assert_eq!(control.attach(&operator), Err(AttachRefused::OtherController), "an evicted operator cannot attach again");
        assert_eq!(control.attach(&other), Err(AttachRefused::OtherController), "the run's controller holds the session again");
        assert_eq!(control.connections(), 1);
        assert_eq!(control.attach(&run), Ok(()));

        control.detach(&operator);
        assert!(control.evicted(&operator), "the evicted operator still has a socket");
        control.detach(&operator);
        assert!(!control.evicted(&operator));
        assert_eq!(control.connections(), 2);
        assert_eq!(control.attach(&operator), Err(AttachRefused::OtherController), "refused again as another controller");
        control.resume_run();
        assert_eq!(control.connections(), 2, "resuming a run that was not waiting evicts no one");
        control.stand_aside(&run);
        control.run_ended();
        assert_eq!(control.attach(&operator), Err(AttachRefused::OtherController), "an ended run leaves no exception behind");
    }

    #[test]
    fn a_run_that_ends_while_waiting_leaves_the_operator_the_session() {
        let (run, operator, other) = (Controller::new("run"), Controller::new("operator"), Controller::new("other"));
        let mut control = Control::default();
        control.attach(&run).unwrap();
        control.stand_aside(&run);
        control.attach(&operator).unwrap();
        control.run_ended();
        control.detach(&run);
        assert!(!control.evicted(&operator));
        assert_eq!(control.connections(), 1);
        assert_eq!(control.attach(&other), Err(AttachRefused::OtherController));
        assert_eq!(control.attach(&operator), Ok(()));
    }

    #[test]
    fn a_takeover_goes_ahead_while_only_the_waiting_runs_controller_is_attached() {
        let (viewer, other) = viewers();
        let (run, operator) = (Controller::new("run"), Controller::new("operator"));
        let mut control = Control::default();
        control.attach(&run).unwrap();
        assert_eq!(control.take(&viewer), Err(TakeRefused::ControllerAttached { connections: 1 }), "a running run's controller refuses it");
        control.stand_aside(&run);
        control.attach(&operator).unwrap();
        assert_eq!(control.take(&viewer), Err(TakeRefused::ControllerAttached { connections: 1 }), "an operator's does too");
        control.detach(&operator);
        assert_eq!(control.take(&viewer), Ok(()));
        assert_eq!(control.authority(), &Authority::Human(viewer.clone()));
        assert_eq!(control.attach(&operator), Err(AttachRefused::Person), "no one attaches beside a person");
        assert_eq!(control.take(&other), Err(TakeRefused::Contested));
        assert_eq!(control.resume(&viewer), Ok(()));
        control.resume_run();
        assert_eq!(control.authority(), &Authority::Automation);
        assert_eq!(control.take(&other), Err(TakeRefused::ControllerAttached { connections: 1 }), "the resumed run's controller holds the session");
    }

    #[test]
    fn a_stopped_session_forgets_a_waiting_run() {
        let (run, operator) = (Controller::new("run"), Controller::new("operator"));
        let mut control = Control::default();
        control.attach(&run).unwrap();
        control.stand_aside(&run);
        control.reset();
        control.attach(&run).unwrap();
        assert_eq!(control.attach(&operator), Err(AttachRefused::OtherController));
    }
}
