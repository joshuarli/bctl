use crate::conn::Conn;
use crate::device;
use crate::effect::Effect;
use crate::wire::*;
use rustix::fd::OwnedFd;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClientState {
    New,
    Active,
    PendingDisable,
    Disabled,
    Closed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceType {
    Evdev,
    Hidraw,
    Drm,
    Wscons,
}

pub struct SeatDevice {
    pub device_id: i32,
    pub fd: Option<OwnedFd>,
    pub ref_cnt: u32,
    pub active: bool,
    pub path: String,
    pub dev_type: DeviceType,
}

pub struct Client {
    pub pid: libc::pid_t,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub session: i32,
    pub state: ClientState,
    pub seat: Option<usize>,
    pub devices: Vec<SeatDevice>,
}

impl Client {
    pub fn fresh(pid: libc::pid_t, uid: libc::uid_t, gid: libc::gid_t) -> Client {
        Client {
            pid,
            uid,
            gid,
            session: -1,
            state: ClientState::New,
            seat: None,
            devices: Vec::new(),
        }
    }
}

pub struct ClientSlot {
    pub conn: Conn,
    pub client: Client,
    pub dead: bool,
}

pub struct Seat {
    pub name: String,
    pub vt_bound: bool,
    pub cur_vt: i32,
    pub clients: Vec<usize>,
    pub active_client: Option<usize>,
    pub next_client: Option<usize>,
}

impl Seat {
    pub fn new(name: &str, vt_bound: bool) -> Seat {
        Seat {
            name: name.to_string(),
            vt_bound,
            cur_vt: 0,
            clients: Vec::new(),
            active_client: None,
            next_client: None,
        }
    }
}

pub fn vt_bound_from_env() -> bool {
    match std::env::var("SEATD_VTBOUND") {
        Err(_) => true,
        Ok(v) => v == "1",
    }
}

pub(crate) fn errno_last() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EINVAL)
}

fn send_error(conn: &mut Conn, code: i32) -> Result<(), i32> {
    let hdr = Header {
        opcode: SERVER_ERROR,
        size: 4,
    };
    conn.put(&hdr.encode())?;
    conn.put(&code.to_ne_bytes())?;
    Ok(())
}

fn send_simple(conn: &mut Conn, opcode: u16) -> Result<(), i32> {
    let hdr = Header { opcode, size: 0 };
    conn.put(&hdr.encode())?;
    Ok(())
}

fn flush_client(conn: &mut Conn) -> Result<(), i32> {
    match conn.flush() {
        Ok(_) => Ok(()),
        Err(e) if e == libc::EAGAIN => Ok(()),
        Err(e) => Err(e),
    }
}

fn send_enable(conn: &mut Conn) -> Result<(), i32> {
    send_simple(conn, SERVER_ENABLE_SEAT)?;
    flush_client(conn)
}

fn send_disable(conn: &mut Conn) -> Result<(), i32> {
    send_simple(conn, SERVER_DISABLE_SEAT)?;
    flush_client(conn)
}

pub fn seat_add_client(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
) -> Result<(), i32> {
    if slots[idx].client.seat.is_some() {
        return Err(libc::EBUSY);
    }
    if seat.vt_bound {
        if let Some(active) = seat.active_client {
            if slots[active].client.state != ClientState::PendingDisable {
                return Err(libc::EBUSY);
            }
        }
    }
    if slots[idx].client.session != -1 {
        return Err(libc::EINVAL);
    }
    Ok(())
}

fn seat_add_client_assign(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    seat_add_client(seat, slots, idx)?;
    if seat.vt_bound {
        let cur = match fx.current_vt() {
            Ok(v) => v,
            Err(_) => return Err(libc::EINVAL),
        };
        if cur == -1 {
            return Err(libc::EINVAL);
        }
        seat.cur_vt = cur;
        if seat.active_client.is_some() {
            for c in seat.clients.iter() {
                if slots[*c].client.session == cur {
                    return Err(libc::EBUSY);
                }
            }
        }
        slots[idx].client.session = cur;
    } else {
        let mut next = 1i32;
        loop {
            let mut collision = None;
            for c in seat.clients.iter() {
                if slots[*c].client.session == next {
                    collision = Some(*c);
                    break;
                }
            }
            match collision {
                None => break,
                Some(_) => next += 1,
            }
        }
        slots[idx].client.session = next;
    }
    slots[idx].client.seat = Some(0);
    seat.clients.insert(0, idx);
    crate::log_info!("VT open: client={0} session={1}", idx, slots[idx].client.session);
    Ok(())
}

fn seat_open_client(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    let state = slots[idx].client.state;
    if state != ClientState::New && state != ClientState::Disabled {
        return Err(libc::EALREADY);
    }
    if seat.active_client.is_some() {
        return Err(libc::EBUSY);
    }
    let session = slots[idx].client.session;
    if seat.vt_bound {
        fx.vt_open(session)?;
    }
    for dev in 0..slots[idx].client.devices.len() {
        if device::activate_device(slots, idx, dev, fx).is_err() {
            crate::log_error!("Could not activate device, continuing");
        }
    }
    if let Err(e) = send_enable(&mut slots[idx].conn) {
        for dev in 0..slots[idx].client.devices.len() {
            let _ = device::deactivate_device(slots, idx, dev, fx);
        }
        if seat.vt_bound {
            let _ = fx.vt_close(session);
        }
        return Err(e);
    }
    slots[idx].client.state = ClientState::Active;
    seat.active_client = Some(idx);
    Ok(())
}

fn seat_activate(seat: &mut Seat, slots: &mut [ClientSlot], fx: &mut dyn Effect) -> Result<(), i32> {
    if seat.active_client.is_some() {
        return Ok(());
    }
    let next: Option<usize> = if seat.next_client.is_some() {
        seat.next_client.take()
    } else if seat.clients.is_empty() || (seat.vt_bound && seat.cur_vt == -1) {
        return Err(errno_last());
    } else if seat.vt_bound {
        let mut found = None;
        for c in seat.clients.iter() {
            if slots[*c].client.session == seat.cur_vt {
                found = Some(*c);
                break;
            }
        }
        match found {
            Some(c) => Some(c),
            None => return Err(errno_last()),
        }
    } else {
        Some(seat.clients[0])
    };
    match next {
        Some(n) => seat_open_client(seat, slots, n, fx),
        None => Err(libc::ENOENT),
    }
}

pub fn seat_remove_client(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) {
    if seat.next_client == Some(idx) {
        seat.next_client = None;
    }
    seat.clients.retain(|c| *c != idx);
    while !slots[idx].client.devices.is_empty() {
        device::close_device(slots, idx, 0, fx);
    }
    let was_current = seat.active_client == Some(idx);
    if was_current {
        seat.active_client = None;
        let _ = seat_activate(seat, slots, fx);
    }
    let state = slots[idx].client.state;
    let session = slots[idx].client.session;
    if seat.vt_bound && ((was_current && seat.active_client.is_none())
        || (!was_current && !matches!(state, ClientState::New | ClientState::Closed))) {
        let _ = fx.vt_close(session);
    }
    slots[idx].client.state = ClientState::Closed;
    slots[idx].client.seat = None;
}

fn seat_disable_client(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.state != ClientState::Active {
        return Err(libc::EBUSY);
    }
    assert_eq!(seat.active_client, Some(idx));
    for dev in 0..slots[idx].client.devices.len() {
        if device::deactivate_device(slots, idx, dev, fx).is_err() {
            crate::log_error!("Could not deactivate device, continuing");
        }
    }
    slots[idx].client.state = ClientState::PendingDisable;
    send_disable(&mut slots[idx].conn)?;
    Ok(())
}

fn seat_ack_disable_client(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.state != ClientState::PendingDisable {
        return Err(libc::EBUSY);
    }
    slots[idx].client.state = ClientState::Disabled;
    if seat.active_client != Some(idx) {
        return Ok(());
    }
    seat.active_client = None;
    let _ = seat_activate(seat, slots, fx);
    Ok(())
}

fn seat_set_next_session(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    session: i32,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.state != ClientState::Active {
        return Err(libc::EPERM);
    }
    assert_eq!(seat.active_client, Some(idx));
    if session <= 0 {
        return Err(libc::EINVAL);
    }
    if session == slots[idx].client.session {
        return Ok(());
    }
    if seat.next_client.is_some() {
        return Ok(());
    }
    if seat.vt_bound {
        crate::log_info!("VT switch requested: {0} -> {1}", seat.cur_vt, session);
        fx.vt_switch(seat.cur_vt, session)?;
        return Ok(());
    }
    let mut target = None;
    for c in seat.clients.iter() {
        if slots[*c].client.session == session {
            target = Some(*c);
            break;
        }
    }
    match target {
        None => Err(libc::EINVAL),
        Some(t) => {
            seat.next_client = Some(t);
            seat_disable_client(seat, slots, idx, fx)
        }
    }
}

pub fn seat_vt_activate(seat: &mut Seat, slots: &mut [ClientSlot], fx: &mut dyn Effect) {
    if !seat.vt_bound {
        return;
    }
    match fx.current_vt() {
        Ok(v) => seat.cur_vt = v,
        Err(_) => return,
    }
    crate::log_info!("VT acquire on {0}, active={1:?}", seat.cur_vt, seat.active_client);
    let _ = fx.vt_ack(seat.cur_vt, false);
    if seat.active_client.is_none() {
        if let Err(e) = seat_activate(seat, slots, fx) {
            crate::log_error!("VT acquire found no session on vt {0}: {1}", seat.cur_vt, std::io::Error::from_raw_os_error(e));
        }
    }
}

pub fn seat_vt_release(seat: &mut Seat, slots: &mut [ClientSlot], fx: &mut dyn Effect) {
    if !seat.vt_bound {
        return;
    }
    if let Ok(v) = fx.current_vt() {
        seat.cur_vt = v;
    }
    crate::log_info!("VT release on {0}, active={1:?}", seat.cur_vt, seat.active_client);
    if let Some(active) = seat.active_client {
        let _ = seat_disable_client(seat, slots, active, fx);
    }
    let _ = fx.vt_ack(seat.cur_vt, true);
    seat.cur_vt = -1;
}

fn handle_open_seat(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if seat_add_client_assign(seat, slots, idx, fx).is_err() {
        return Err(-1);
    }
    let name = seat.name.clone() + "\0";
    let bytes = name.as_bytes();
    let hdr = Header {
        opcode: SERVER_SEAT_OPENED,
        size: (2 + bytes.len()) as u16,
    };
    let rmsg = (bytes.len() as u16).to_ne_bytes();
    if slots[idx].conn.put(&hdr.encode()).is_err()
        || slots[idx].conn.put(&rmsg).is_err()
        || slots[idx].conn.put(bytes).is_err()
    {
        return Err(-1);
    }
    if seat.active_client.is_none() {
        if let Err(e) = seat_open_client(seat, slots, idx, fx) {
            crate::log_error!("Could not activate client: {}", std::io::Error::from_raw_os_error(e));
            return Err(-1);
        }
    }
    Ok(())
}

fn handle_close_seat(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.seat.is_none() {
        return Err(-1);
    }
    seat_remove_client(seat, slots, idx, fx);
    send_simple(&mut slots[idx].conn, SERVER_SEAT_CLOSED).map_err(|_| -1)?;
    Ok(())
}

fn handle_open_device(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    size: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.seat.is_none() || size < 2 {
        return Err(-1);
    }
    let mut msg = [0u8; 2];
    if slots[idx].conn.get(&mut msg).is_err() {
        return Err(-1);
    }
    let path_len = u16::from_ne_bytes([msg[0], msg[1]]) as usize;
    if path_len != size - 2 {
        return Err(-1);
    }
    if path_len > MAX_PATH_LEN {
        return Err(-1);
    }
    let mut path = vec![0u8; path_len];
    if slots[idx].conn.get(&mut path).is_err() {
        return Err(-1);
    }
    if path_len == 0 || memchr_nul(&path) != path_len - 1 {
        return Err(-1);
    }
    let path_str = match std::str::from_utf8(&path[..path_len - 1]) {
        Ok(s) => s.to_string(),
        Err(_) => return Err(-1),
    };
    let _ = seat;
    match device::open_device(seat.active_client, slots, idx, &path_str, fx) {
        Ok((device_id, dupfd)) => {
            if slots[idx].conn.put_fd(dupfd).is_err() {
                return Err(-1);
            }
            let hdr = Header {
                opcode: SERVER_DEVICE_OPENED,
                size: 4,
            };
            if slots[idx].conn.put(&hdr.encode()).is_err()
                || slots[idx].conn.put(&device_id.to_ne_bytes()).is_err()
            {
                return Err(-1);
            }
            Ok(())
        }
        Err(e) => {
            let _ = send_error(&mut slots[idx].conn, e);
            Ok(())
        }
    }
}

fn memchr_nul(path: &[u8]) -> usize {
    for (i, b) in path.iter().enumerate() {
        if *b == 0 {
            return i;
        }
    }
    path.len()
}

fn handle_close_device(
    slots: &mut [ClientSlot],
    idx: usize,
    size: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.seat.is_none() {
        return Err(-1);
    }
    let mut msg = [0u8; 4];
    if size != 4 || slots[idx].conn.get(&mut msg).is_err() {
        return Err(-1);
    }
    let device_id = i32::from_ne_bytes([msg[0], msg[1], msg[2], msg[3]]);
    match device::find_device(slots, idx, device_id) {
        Err(_) => {
            let _ = send_error(&mut slots[idx].conn, libc::EBADF);
            Ok(())
        }
        Ok(dev) => {
            device::close_device(slots, idx, dev, fx);
            send_simple(&mut slots[idx].conn, SERVER_DEVICE_CLOSED).map_err(|_| -1)?;
            Ok(())
        }
    }
}

fn handle_switch_session(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    size: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.seat.is_none() {
        return Err(-1);
    }
    let mut msg = [0u8; 4];
    if size != 4 || slots[idx].conn.get(&mut msg).is_err() {
        return Err(-1);
    }
    let session = i32::from_ne_bytes([msg[0], msg[1], msg[2], msg[3]]);
    if let Err(code) = seat_set_next_session(seat, slots, idx, session, fx) {
        let _ = send_error(&mut slots[idx].conn, code);
        return Ok(());
    }
    send_simple(&mut slots[idx].conn, SERVER_SESSION_SWITCHED).map_err(|_| -1)?;
    Ok(())
}

fn handle_disable_seat(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    fx: &mut dyn Effect,
) -> Result<(), i32> {
    if slots[idx].client.seat.is_none() {
        return Err(-1);
    }
    if let Err(code) = seat_ack_disable_client(seat, slots, idx, fx) {
        let _ = send_error(&mut slots[idx].conn, code);
        return Ok(());
    }
    send_simple(&mut slots[idx].conn, SERVER_SEAT_DISABLED).map_err(|_| -1)?;
    Ok(())
}

fn handle_ping(slots: &mut [ClientSlot], idx: usize) -> Result<(), i32> {
    send_simple(&mut slots[idx].conn, SERVER_PONG).map_err(|_| -1)?;
    Ok(())
}

pub enum Outcome {
    Done,
    Destroy,
}

pub fn handle_opcode(
    seat: &mut Seat,
    slots: &mut [ClientSlot],
    idx: usize,
    opcode: u16,
    size: usize,
    fx: &mut dyn Effect,
) -> Outcome {
    let r = match opcode {
        CLIENT_OPEN_SEAT => {
            if size != 0 {
                Err(-1)
            } else {
                handle_open_seat(seat, slots, idx, fx)
            }
        }
        CLIENT_CLOSE_SEAT => {
            if size != 0 {
                Err(-1)
            } else {
                handle_close_seat(seat, slots, idx, fx)
            }
        }
        CLIENT_OPEN_DEVICE => handle_open_device(seat, slots, idx, size, fx),
        CLIENT_CLOSE_DEVICE => handle_close_device(slots, idx, size, fx),
        CLIENT_SWITCH_SESSION => handle_switch_session(seat, slots, idx, size, fx),
        CLIENT_DISABLE_SEAT => {
            if size != 0 {
                Err(-1)
            } else {
                handle_disable_seat(seat, slots, idx, fx)
            }
        }
        CLIENT_PING => {
            if size != 0 {
                Err(-1)
            } else {
                handle_ping(slots, idx)
            }
        }
        _ => Err(-1),
    };
    match r {
        Ok(()) => match flush_client(&mut slots[idx].conn) {
            Ok(()) => Outcome::Done,
            Err(_) => Outcome::Destroy,
        },
        Err(_) => Outcome::Destroy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;
    use crate::effect::NullEffect;

    fn pair_conns() -> (Conn, Conn) {
        unsafe {
            let mut fds = [0; 2];
            assert_eq!(
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    0,
                    fds.as_mut_ptr()
                ),
                0
            );
            (
                Conn::new(OwnedFd::from_raw_fd(fds[0])),
                Conn::new(OwnedFd::from_raw_fd(fds[1])),
            )
        }
    }

    fn setup(vt_bound: bool, cur_vt: i32) -> (Seat, Vec<ClientSlot>, NullEffect) {
        (
            Seat::new("seat0", vt_bound),
            Vec::new(),
            NullEffect::new(cur_vt),
        )
    }

    fn add_client(slots: &mut Vec<ClientSlot>) -> (usize, Conn) {
        let (a, b) = pair_conns();
        slots.push(ClientSlot {
            conn: a,
            client: Client::fresh(1000, 1000, 1000),
            dead: false,
        });
        (slots.len() - 1, b)
    }

    fn send(
        seat: &mut Seat,
        slots: &mut [ClientSlot],
        idx: usize,
        peer: &mut Conn,
        opcode: u16,
        body: &[u8],
        fx: &mut dyn Effect,
    ) -> Outcome {
        peer.put(&Header { opcode, size: body.len() as u16 }.encode()).unwrap();
        peer.put(body).unwrap();
        peer.flush().unwrap();
        slots[idx].conn.read().unwrap();
        let mut raw = [0u8; 4];
        slots[idx].conn.get(&mut raw).unwrap();
        let hdr = Header::decode(&raw);
        assert_eq!((hdr.opcode, hdr.size as usize), (opcode, body.len()));
        handle_opcode(seat, slots, idx, hdr.opcode, hdr.size as usize, fx)
    }

    fn read_msg(peer: &mut Conn) -> (u16, Vec<u8>) {
        loop {
            if peer.pending() >= 4 {
                break;
            }
            peer.read().unwrap();
        }
        let mut raw = [0u8; 4];
        peer.get(&mut raw).unwrap();
        let hdr = Header::decode(&raw);
        loop {
            if peer.pending() >= hdr.size as usize {
                break;
            }
            peer.read().unwrap();
        }
        let mut body = vec![0u8; hdr.size as usize];
        peer.get(&mut body).unwrap();
        (hdr.opcode, body)
    }

    fn open_seat(
        seat: &mut Seat,
        slots: &mut [ClientSlot],
        idx: usize,
        peer: &mut Conn,
        fx: &mut dyn Effect,
    ) -> Outcome {
        send(seat, slots, idx, peer, CLIENT_OPEN_SEAT, &[], fx)
    }

    fn open_drain(
        seat: &mut Seat,
        slots: &mut [ClientSlot],
        idx: usize,
        peer: &mut Conn,
        fx: &mut dyn Effect,
        expect_enable: bool,
    ) -> Outcome {
        let out = open_seat(seat, slots, idx, peer, fx);
        if matches!(out, Outcome::Done) {
            let (op, _) = read_msg(peer);
            assert_eq!(op, SERVER_SEAT_OPENED);
            if expect_enable {
                let (op, _) = read_msg(peer);
                assert_eq!(op, SERVER_ENABLE_SEAT);
            }
        }
        out
    }

    #[test]
    fn session_backpressure_preserves_enable_state() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        let mut queued = 0;
        let filler = [0x55u8; 1024];
        loop {
            let n = unsafe { libc::send(slots[a].conn.raw_fd(), filler.as_ptr().cast(), filler.len(), libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) };
            if n < 0 {
                assert_eq!(errno_last(), libc::EAGAIN);
                break;
            }
            assert!(n > 0);
            queued += n as usize;
            assert!(queued < 4 * 1024 * 1024);
        }
        assert!(matches!(open_seat(&mut seat, &mut slots, a, &mut pa, &mut fx), Outcome::Done));
        assert_eq!(seat.active_client, Some(a));
        assert_eq!(slots[a].client.state, ClientState::Active);
        while queued > 0 {
            let mut drain = [0u8; 1024];
            let n = unsafe { libc::recv(pa.raw_fd(), drain.as_mut_ptr().cast(), queued.min(drain.len()), libc::MSG_DONTWAIT) };
            assert!(n > 0);
            queued -= n as usize;
        }
        slots[a].conn.flush().unwrap();
        assert_eq!(read_msg(&mut pa).0, SERVER_SEAT_OPENED);
        assert_eq!(read_msg(&mut pa).0, SERVER_ENABLE_SEAT);
    }

    #[test]
    fn session_open_assigns_incremental_ids() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_seat(&mut seat, &mut slots, a, &mut pa, &mut fx), Outcome::Done));
        let (op, body) = read_msg(&mut pa);
        assert_eq!(op, SERVER_SEAT_OPENED);
        assert!(body.ends_with(b"seat0\0"));
        assert_eq!(slots[a].client.session, 1);
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, false), Outcome::Done));
        assert_eq!(slots[b].client.session, 2);
        assert_eq!(seat.active_client, Some(a));
        assert_eq!(slots[b].client.state, ClientState::New);
    }

    #[test]
    fn session_ids_reused_after_close() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        seat_remove_client(&mut seat, &mut slots, a, &mut fx);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, true), Outcome::Done));
        assert_eq!(slots[b].client.session, 1);
    }

    #[test]
    fn session_reopen_same_connection_destroys() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert!(matches!(open_seat(&mut seat, &mut slots, a, &mut pa, &mut fx), Outcome::Destroy));
    }

    #[test]
    fn session_vtbound_second_client_ebusy() {
        let (mut seat, mut slots, mut fx) = setup(true, 3);
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert_eq!(slots[a].client.session, 3);
        assert!(matches!(
            send(&mut seat, &mut slots, b, &mut pb, CLIENT_OPEN_SEAT, &[], &mut fx),
            Outcome::Destroy
        ));
        assert!(fx.calls.iter().any(|c| c == "vt_open:3"));
    }

    #[test]
    fn session_ping_pong_and_bad_size() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(send(&mut seat, &mut slots, a, &mut pa, CLIENT_PING, &[], &mut fx), Outcome::Done));
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_PING, &[1u8], &mut fx),
            Outcome::Destroy
        ));
    }

    #[test]
    fn session_unknown_opcode_destroys() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(send(&mut seat, &mut slots, a, &mut pa, 99, &[], &mut fx), Outcome::Destroy));
    }

    #[test]
    fn session_open_device_needs_active() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, false), Outcome::Done));
        let mut req = 10u16.to_ne_bytes().to_vec();
        req.extend_from_slice(b"/dev/null\0");
        assert!(matches!(
            send(&mut seat, &mut slots, b, &mut pb, CLIENT_OPEN_DEVICE, &req, &mut fx),
            Outcome::Done
        ));
        let (op, body) = read_msg(&mut pb);
        assert_eq!(op, SERVER_ERROR);
        assert_eq!(i32::from_ne_bytes(body[..4].try_into().unwrap()), libc::EPERM);
    }

    #[test]
    fn session_open_device_bad_path_rules() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        for req in [
            [5u16.to_ne_bytes().to_vec(), b"/x\0\0".to_vec()].concat(),
            [9u16.to_ne_bytes().to_vec(), b"/dev/nullX".to_vec()].concat(),
            [9u16.to_ne_bytes().to_vec()].concat(),
            [0u16.to_ne_bytes().to_vec()].concat(),
        ] {
            let (c, mut pc) = add_client(&mut slots);
            assert!(matches!(
                send(&mut seat, &mut slots, c, &mut pc, CLIENT_OPEN_DEVICE, &req, &mut fx),
                Outcome::Destroy
            ));
        }
    }

    #[test]
    fn session_close_device_unknown_sends_ebadf() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        for id in [0i32, 7] {
            assert!(matches!(
                send(&mut seat, &mut slots, a, &mut pa, CLIENT_CLOSE_DEVICE, &id.to_ne_bytes(), &mut fx),
                Outcome::Done
            ));
            let (op, body) = read_msg(&mut pa);
            assert_eq!(op, SERVER_ERROR);
            assert_eq!(i32::from_ne_bytes(body[..4].try_into().unwrap()), libc::EBADF);
        }
    }

    #[test]
    fn session_device_ids_are_client_scoped() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (_a, _) = add_client(&mut slots);
        let (_b, _) = add_client(&mut slots);
        slots[0].client.state = ClientState::Active;
        slots[0].client.seat = Some(0);
        seat.clients.push(0);
        seat.active_client = Some(0);
        let (id1, _f1) = device::open_device_resolved(seat.active_client, &mut slots, 0, "/dev/input/event0", DeviceType::Evdev, &mut fx).unwrap();
        let (id2, _f2) = device::open_device_resolved(seat.active_client, &mut slots, 0, "/dev/input/event0", DeviceType::Evdev, &mut fx).unwrap();
        assert_eq!((id1, id2), (1, 1));
        assert_eq!(slots[0].client.devices.len(), 1);
        assert_eq!(slots[0].client.devices[0].ref_cnt, 2);
        assert!(device::find_device(&slots, 0, 2).is_err());
        assert!(device::find_device(&slots, 1, 1).is_err());
        assert!(device::find_device(&slots, 0, 1).is_ok());
        device::close_device(&mut slots, 0, 0, &mut fx);
        assert_eq!(slots[0].client.devices[0].ref_cnt, 1);
        device::close_device(&mut slots, 0, 0, &mut fx);
        assert!(slots[0].client.devices.is_empty());
    }

    #[test]
    fn session_device_exhaustion_is_emfile() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, _) = add_client(&mut slots);
        slots[a].client.state = ClientState::Active;
        slots[a].client.seat = Some(0);
        seat.clients.push(a);
        seat.active_client = Some(a);
        for i in 0..MAX_SEAT_DEVICES {
            let p = format!("/dev/input/event{i}");
            device::open_device_resolved(seat.active_client, &mut slots, a, &p, DeviceType::Evdev, &mut fx).unwrap();
        }
        let r = device::open_device_resolved(seat.active_client, &mut slots, a, "/dev/input/event999", DeviceType::Evdev, &mut fx);
        assert_eq!(r.unwrap_err(), libc::EMFILE);
    }

    struct FailRevoke {
        null: NullEffect,
        failures: usize,
    }

    impl Effect for FailRevoke {
        fn open_device(&mut self, path: &str, t: DeviceType) -> Result<rustix::fd::OwnedFd, i32> {
            self.null.open_device(path, t)
        }
        fn activate_device(&mut self, fd: std::os::unix::io::RawFd, t: DeviceType) -> Result<(), i32> {
            self.null.activate_device(fd, t)
        }
        fn deactivate_device(&mut self, _fd: std::os::unix::io::RawFd, _t: DeviceType) -> Result<(), i32> {
            self.failures += 1;
            Err(libc::ENODEV)
        }
        fn vt_open(&mut self, s: i32) -> Result<(), i32> {
            self.null.vt_open(s)
        }
        fn vt_close(&mut self, s: i32) -> Result<(), i32> {
            self.null.vt_close(s)
        }
        fn vt_switch(&mut self, c: i32, t: i32) -> Result<(), i32> {
            self.null.vt_switch(c, t)
        }
        fn vt_ack(&mut self, c: i32, r: bool) -> Result<(), i32> {
            self.null.vt_ack(c, r)
        }
        fn current_vt(&mut self) -> Result<i32, i32> {
            self.null.current_vt()
        }
    }

    #[test]
    fn session_revoke_failure_across_full_switch() {
        let (mut seat, mut slots, null) = setup(false, 0);
        let mut fx = FailRevoke { null, failures: 0 };
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, false), Outcome::Done));
        device::open_device_resolved(seat.active_client, &mut slots, a, "/dev/input/event0", DeviceType::Evdev, &mut fx).unwrap();
        assert!(slots[a].client.devices[0].active);
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_SWITCH_SESSION, &2i32.to_ne_bytes(), &mut fx),
            Outcome::Done
        ));
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_DISABLE_SEAT);
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_SESSION_SWITCHED);
        assert_eq!(fx.failures, 1, "revoke must be attempted exactly once");
        assert!(
            slots[a].client.devices[0].active,
            "failed revoke must not clear the active flag"
        );
        assert_eq!(slots[a].client.state, ClientState::PendingDisable);
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_DISABLE_SEAT, &[], &mut fx),
            Outcome::Done
        ));
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_SEAT_DISABLED);
        let (op, _) = read_msg(&mut pb);
        assert_eq!(op, SERVER_ENABLE_SEAT);
        assert_eq!(slots[b].client.state, ClientState::Active);
        assert_eq!(seat.active_client, Some(b));
    }

    #[test]
    fn session_full_switch_flow() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, false), Outcome::Done));
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_SWITCH_SESSION, &2i32.to_ne_bytes(), &mut fx),
            Outcome::Done
        ));
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_DISABLE_SEAT);
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_SESSION_SWITCHED);
        assert_eq!(slots[a].client.state, ClientState::PendingDisable);
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_DISABLE_SEAT, &[], &mut fx),
            Outcome::Done
        ));
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_SEAT_DISABLED);
        let (op, _) = read_msg(&mut pb);
        assert_eq!(op, SERVER_ENABLE_SEAT);
        assert_eq!(slots[a].client.state, ClientState::Disabled);
        assert_eq!(slots[b].client.state, ClientState::Active);
        assert_eq!(seat.active_client, Some(b));
    }

    #[test]
    fn session_switch_same_and_queued_are_noops() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, false), Outcome::Done));
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_SWITCH_SESSION, &1i32.to_ne_bytes(), &mut fx),
            Outcome::Done
        ));
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_SESSION_SWITCHED);
        assert_eq!(seat.next_client, None);
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_SWITCH_SESSION, &0i32.to_ne_bytes(), &mut fx),
            Outcome::Done
        ));
        let (op, body) = read_msg(&mut pa);
        assert_eq!(op, SERVER_ERROR);
        assert_eq!(i32::from_ne_bytes(body[..4].try_into().unwrap()), libc::EINVAL);
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_SWITCH_SESSION, &99i32.to_ne_bytes(), &mut fx),
            Outcome::Done
        ));
        let (op, body) = read_msg(&mut pa);
        assert_eq!(op, SERVER_ERROR);
        assert_eq!(i32::from_ne_bytes(body[..4].try_into().unwrap()), libc::EINVAL);
    }

    #[test]
    fn session_disable_unpending_sends_ebusy() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_DISABLE_SEAT, &[], &mut fx),
            Outcome::Done
        ));
        let (op, body) = read_msg(&mut pa);
        assert_eq!(op, SERVER_ERROR);
        assert_eq!(i32::from_ne_bytes(body[..4].try_into().unwrap()), libc::EBUSY);
    }

    #[test]
    fn session_disconnect_activates_next() {
        let (mut seat, mut slots, mut fx) = setup(false, 0);
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, false), Outcome::Done));
        seat_remove_client(&mut seat, &mut slots, a, &mut fx);
        assert_eq!(seat.active_client, Some(b));
        assert_eq!(slots[b].client.state, ClientState::Active);
        let (op, _) = read_msg(&mut pb);
        assert_eq!(op, SERVER_ENABLE_SEAT);
    }

    #[test]
    fn session_vt_setup_failure_disconnects_client() {
        struct FailedVt;
        impl Effect for FailedVt {
            fn open_device(&mut self, _: &str, _: DeviceType) -> Result<OwnedFd, i32> { unreachable!() }
            fn activate_device(&mut self, _: i32, _: DeviceType) -> Result<(), i32> { unreachable!() }
            fn deactivate_device(&mut self, _: i32, _: DeviceType) -> Result<(), i32> { unreachable!() }
            fn vt_open(&mut self, _: i32) -> Result<(), i32> { Err(libc::EIO) }
            fn vt_close(&mut self, _: i32) -> Result<(), i32> { panic!("closing a VT that was never acquired") }
            fn vt_switch(&mut self, _: i32, _: i32) -> Result<(), i32> { unreachable!() }
            fn vt_ack(&mut self, _: i32, _: bool) -> Result<(), i32> { unreachable!() }
            fn current_vt(&mut self) -> Result<i32, i32> { Ok(1) }
        }
        let (mut seat, mut slots, _) = setup(true, 1);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(open_seat(&mut seat, &mut slots, a, &mut pa, &mut FailedVt), Outcome::Destroy));
        assert_eq!(seat.active_client, None);
        seat_remove_client(&mut seat, &mut slots, a, &mut FailedVt);
    }

    #[test]
    fn session_vt_open_during_release_activates_after_ack() {
        let (mut seat, mut slots, mut fx) = setup(true, 1);
        let (a, mut pa) = add_client(&mut slots);
        let (b, mut pb) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        seat_vt_release(&mut seat, &mut slots, &mut fx);
        assert_eq!(read_msg(&mut pa).0, SERVER_DISABLE_SEAT);
        fx.cur_vt = 2;
        assert!(matches!(open_drain(&mut seat, &mut slots, b, &mut pb, &mut fx, false), Outcome::Done));
        assert!(matches!(send(&mut seat, &mut slots, a, &mut pa, CLIENT_DISABLE_SEAT, &[], &mut fx), Outcome::Done));
        assert_eq!(seat.active_client, Some(b));
        assert_eq!(read_msg(&mut pa).0, SERVER_SEAT_DISABLED);
        assert_eq!(read_msg(&mut pb).0, SERVER_ENABLE_SEAT);
    }

    #[test]
    fn session_vt_release_and_acquire() {
        let (mut seat, mut slots, mut fx) = setup(true, 5);
        let (a, mut pa) = add_client(&mut slots);
        assert!(matches!(open_drain(&mut seat, &mut slots, a, &mut pa, &mut fx, true), Outcome::Done));
        seat_vt_release(&mut seat, &mut slots, &mut fx);
        assert_eq!(slots[a].client.state, ClientState::PendingDisable);
        assert_eq!(seat.cur_vt, -1);
        let (op, _) = read_msg(&mut pa);
        assert_eq!(op, SERVER_DISABLE_SEAT);
        assert!(matches!(
            send(&mut seat, &mut slots, a, &mut pa, CLIENT_DISABLE_SEAT, &[], &mut fx),
            Outcome::Done
        ));
        let _ = read_msg(&mut pa);
        fx.cur_vt = 7;
        seat_vt_activate(&mut seat, &mut slots, &mut fx);
        assert_eq!(seat.cur_vt, 7);
        assert!(fx.calls.iter().any(|c| c == "vt_ack:7:0"));
    }
}
