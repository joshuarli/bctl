//! Brokered seat devices through the system libseat client, seatd-only.
//!
//! Links libseat at build time (host provides libseat-dev, the guest image
//! provides libseat). LIBSEAT_BACKEND must select seatd; any other backend,
//! a failed open, or a lost daemon is a hard error, never a silent
//! direct-open fallback.
//!
//! This is the only module in src/ that uses unsafe. The surface is nine
//! declared libseat symbols and two listener callbacks. All raw pointers
//! stay inside Session; Session is !Send and !Sync.

use core::ffi::{c_char, c_int, c_void};
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

#[link(name = "seat", kind = "dylib")]
extern "C" {
    fn libseat_open_seat(listener: *const SeatListener, userdata: *mut c_void) -> *mut Libseat;
    fn libseat_disable_seat(seat: *mut Libseat) -> c_int;
    fn libseat_close_seat(seat: *mut Libseat) -> c_int;
    fn libseat_open_device(seat: *mut Libseat, path: *const c_char, fd: *mut c_int) -> c_int;
    fn libseat_close_device(seat: *mut Libseat, device_id: c_int) -> c_int;
    fn libseat_seat_name(seat: *mut Libseat) -> *const c_char;
    fn libseat_get_fd(seat: *mut Libseat) -> c_int;
    fn libseat_dispatch(seat: *mut Libseat, timeout: c_int) -> c_int;
    fn libseat_switch_session(seat: *mut Libseat, session: c_int) -> c_int;
}

#[repr(C)]
struct Libseat {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Debug)]
struct SeatListener {
    enable_seat: Option<unsafe extern "C" fn(*mut Libseat, *mut c_void)>,
    disable_seat: Option<unsafe extern "C" fn(*mut Libseat, *mut c_void)>,
}

#[derive(Debug)]
struct ListenerState {
    enabled: AtomicBool,
    disabled: AtomicBool,
}

unsafe extern "C" fn enable_cb(_seat: *mut Libseat, userdata: *mut c_void) {
    // SAFETY: userdata is our ListenerState, alive while the session is
    // open; this callback only runs during dispatch on the owning thread.
    let state = &*(userdata as *const ListenerState);
    state.enabled.store(true, Ordering::SeqCst);
}

unsafe extern "C" fn disable_cb(_seat: *mut Libseat, userdata: *mut c_void) {
    // SAFETY: same contract as enable_cb.
    let state = &*(userdata as *const ListenerState);
    state.disabled.store(true, Ordering::SeqCst);
}

fn last_error(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}

/// The daemon is gone when its side of the session socket hangs up.
/// Poll with a zero timeout so this never blocks the event loop.
fn daemon_liveness(fd: BorrowedFd<'_>) -> Result<(), String> {
    let mut fds = [rustix::event::PollFd::new(
        &fd,
        rustix::event::PollFlags::IN,
    )];
    let zero = rustix::time::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match rustix::event::poll(&mut fds, Some(&zero)) {
        Ok(_) => {
            if fds[0]
                .revents()
                .intersects(rustix::event::PollFlags::HUP | rustix::event::PollFlags::ERR)
            {
                return Err("seat daemon lost".to_string());
            }
            Ok(())
        }
        Err(e) => {
            if e == rustix::io::Errno::INTR {
                return Ok(());
            }
            Err(format!("seat poll failed: {e}"))
        }
    }
}

/// An open libseat session. Owns the listener, its state, and the session
/// pointer. The listener and state are boxed: libseat keeps the listener
/// pointer, so both must outlive the session at stable addresses.
#[derive(Debug)]
pub struct Session {
    seat: *mut Libseat,
    _listener: Box<SeatListener>,
    state: Box<ListenerState>,
    fd: OwnedFd,
}

// Session holds raw pointers, so it is !Send and !Sync by default and
// stays confined to its owning thread.

impl Drop for Session {
    fn drop(&mut self) {
        // Best effort: the daemon may already be gone.
        // SAFETY: seat was returned by open_seat and not yet closed.
        unsafe {
            libseat_close_seat(self.seat);
        }
    }
}

impl Session {
    /// Require the seatd backend and open the seat.
    pub fn open() -> Result<Self, String> {
        match std::env::var("LIBSEAT_BACKEND") {
            Ok(selected) if selected == "seatd" => {}
            other => {
                return Err(format!(
                    "LIBSEAT_BACKEND must be seatd for brokered devices, got {other:?}"
                ));
            }
        }
        let mut state = Box::new(ListenerState {
            enabled: AtomicBool::new(false),
            disabled: AtomicBool::new(false),
        });
        let listener = Box::new(SeatListener {
            enable_seat: Some(enable_cb),
            disable_seat: Some(disable_cb),
        });
        // SAFETY: both boxes outlive the session; libseat only reads the
        // listener through dispatch on this thread.
        let seat = unsafe {
            libseat_open_seat(
                &*listener,
                (&mut *state as *mut ListenerState).cast::<c_void>(),
            )
        };
        if seat.is_null() {
            return Err(last_error("libseat_open_seat"));
        }
        // SAFETY: get_fd returns a borrowed daemon fd; dup our own copy.
        let raw = unsafe { libseat_get_fd(seat) };
        if raw < 0 {
            // SAFETY: seat was just opened and is still ours.
            unsafe {
                libseat_close_seat(seat);
            }
            return Err(last_error("libseat_get_fd"));
        }
        let fd = unsafe { BorrowedFd::borrow_raw(raw) }
            .try_clone_to_owned()
            .map_err(|e| format!("dup seat fd: {e}"))?;
        let name: String;
        // SAFETY: seat is open; the name is owned by the session.
        unsafe {
            let p = libseat_seat_name(seat);
            name = if p.is_null() {
                "?".into()
            } else {
                core::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            };
        }
        eprintln!("TINYLAND_SEAT_OPEN seat={name}");
        Ok(Self {
            seat,
            _listener: listener,
            state,
            fd,
        })
    }

    /// Borrow the duplicated session fd for the shared poll set.
    pub fn poll_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Dispatch pending seat events without waiting. Daemon loss is fatal.
    ///
    /// `libseat_dispatch` with a zero timeout reports a dead daemon as an
    /// empty read, so check the session fd for hangup first: without this,
    /// a killed broker is silent and the session never exits.
    pub fn dispatch(&self) -> Result<(), String> {
        daemon_liveness(self.fd.as_fd())?;
        // SAFETY: seat is open; timeout 0 never blocks.
        let n = unsafe { libseat_dispatch(self.seat, 0) };
        if n < 0 {
            return Err(last_error("libseat_dispatch"));
        }
        Ok(())
    }

    /// Dispatch with a bounded wait, for the revoke race: the kernel
    /// reports a revoked device as gone before the daemon's disable
    /// notification arrives, so drain first and let a suspend win over
    /// an unplug verdict. Daemon loss is fatal here too.
    pub fn dispatch_wait(&self, timeout_ms: i32) -> Result<(), String> {
        daemon_liveness(self.fd.as_fd())?;
        // SAFETY: seat is open; timeout bounds the block below.
        let n = unsafe { libseat_dispatch(self.seat, timeout_ms) };
        if n < 0 {
            return Err(last_error("libseat_dispatch"));
        }
        Ok(())
    }

    /// Ask the broker to switch sessions. On a VT-bound seat the session
    /// number is the VT number, so this is how a compositor whose keyboards
    /// are grabbed hands a Ctrl+Alt+Fn switch to the privileged broker.
    /// Failure leaves the session unaffected by design.
    pub fn switch_session(&self, session: i32) -> Result<(), String> {
        // SAFETY: seat is open; the daemon validates the session number.
        let r = unsafe { libseat_switch_session(self.seat, session) };
        if r != 0 {
            return Err(last_error("libseat_switch_session"));
        }
        Ok(())
    }

    /// True after disable_seat until devices are dropped and acknowledged.
    pub fn take_disabled(&self) -> bool {
        self.state.disabled.swap(false, Ordering::SeqCst)
    }

    /// True after enable_seat until the session reopens its devices.
    pub fn take_enabled(&self) -> bool {
        self.state.enabled.swap(false, Ordering::SeqCst)
    }

    /// Acknowledge a disable event after all seat devices are dropped.
    pub fn acknowledge_disable(&self) -> Result<(), String> {
        // SAFETY: seat is open; caller dropped every device first.
        let r = unsafe { libseat_disable_seat(self.seat) };
        if r != 0 {
            return Err(last_error("libseat_disable_seat"));
        }
        Ok(())
    }

    /// Open one brokered device. The returned fd is owned by the caller;
    /// pass the id to close_device when the fd is dropped or revoked.
    pub fn open_device(&self, path: &std::path::Path) -> Result<(OwnedFd, i32), String> {
        let raw = path.as_os_str().as_encoded_bytes();
        if raw.contains(&0) {
            return Err(format!("device path has NUL: {}", path.display()));
        }
        let mut arg = Vec::with_capacity(raw.len() + 1);
        arg.extend_from_slice(raw);
        arg.push(0);
        let mut out: c_int = -1;
        // SAFETY: arg is NUL-terminated; out receives the new fd.
        let id = unsafe {
            libseat_open_device(self.seat, arg.as_ptr() as *const c_char, &mut out)
        };
        if id < 0 {
            return Err(format!(
                "libseat_open_device {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        if out < 0 {
            return Err(format!("libseat returned no fd for {}", path.display()));
        }
        // SAFETY: libseat transfers ownership of the new fd to the caller.
        let fd = unsafe { OwnedFd::from_raw_fd(out) };
        Ok((fd, id))
    }

    /// Release the broker side of a device whose fd was already dropped.
    pub fn close_device(&self, id: i32, origin: &str) -> Result<(), String> {
        // SAFETY: id came from open_device on this session.
        let r = unsafe { libseat_close_device(self.seat, id) };
        if r != 0 {
            return Err(last_error(&format!("libseat_close_device {origin}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn backend_is_required_to_be_seatd() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved = std::env::var_os("LIBSEAT_BACKEND");
        unsafe {
            std::env::remove_var("LIBSEAT_BACKEND");
        }
        assert!(Session::open().unwrap_err().contains("LIBSEAT_BACKEND"));
        unsafe {
            std::env::set_var("LIBSEAT_BACKEND", "builtin");
        }
        assert!(Session::open().unwrap_err().contains("LIBSEAT_BACKEND"));
        match saved {
            Some(v) => unsafe { std::env::set_var("LIBSEAT_BACKEND", v) },
            None => unsafe { std::env::remove_var("LIBSEAT_BACKEND") },
        }
    }

    #[test]
    fn closed_peer_reports_daemon_loss() {
        use std::os::fd::AsFd;
        use std::os::unix::net::UnixStream;
        let (open, peer) = UnixStream::pair().unwrap();
        assert!(daemon_liveness(open.as_fd()).is_ok());
        drop(peer);
        assert!(daemon_liveness(open.as_fd()).is_err());
    }

    #[test]
    fn open_without_daemon_is_a_hard_error() {        let _guard = ENV_LOCK.lock().unwrap();
        let saved = std::env::var_os("LIBSEAT_BACKEND");
        unsafe {
            std::env::set_var("LIBSEAT_BACKEND", "seatd");
            std::env::remove_var("SEATD_SOCK");
        }
        let err = Session::open().unwrap_err();
        assert!(err.contains("libseat"), "unexpected: {err}");
        match saved {
            Some(v) => unsafe { std::env::set_var("LIBSEAT_BACKEND", v) },
            None => unsafe { std::env::remove_var("LIBSEAT_BACKEND") },
        }
    }
}
