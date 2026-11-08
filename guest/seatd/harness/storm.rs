use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Err(e) = self.child.kill() {
            if e.kind() != std::io::ErrorKind::InvalidInput {
                eprintln!("kill daemon: {e}");
            }
        }
        if let Err(e) = self.child.wait() {
            eprintln!("wait daemon: {e}");
        }
    }
}

impl Daemon {
    fn id(&self) -> u32 {
        self.child.id()
    }
}

fn rust_bin() -> String {
    env!("CARGO_BIN_EXE_seatd").to_string()
}

fn unlink(path: &str) {
    unsafe {
        let c = std::ffi::CString::new(path).unwrap();
        libc::unlink(c.as_ptr());
    }
}

fn spawn_rust(sock: &str) -> Daemon {
    unlink(sock);
    let child = Daemon { child: Command::new(rust_bin())
        .arg("-l")
        .arg("silent")
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .env("SEATD_VTBOUND", "0")
        .env("SEATD_SOCK", sock)
        .spawn()
        .expect("spawn rust seatd") };
    for _ in 0..50 {
        if std::path::Path::new(sock).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(std::path::Path::new(sock).exists());
    child
}

struct Raw {
    s: UnixStream,
}

impl Raw {
    fn connect(sock: &str) -> Raw {
        let s = UnixStream::connect(sock).expect("connect");
        s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        s.set_write_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        Raw { s }
    }

    fn send(&mut self, opcode: u16, body: &[u8]) {
        let mut f = Vec::with_capacity(4 + body.len());
        f.extend_from_slice(&opcode.to_ne_bytes());
        f.extend_from_slice(&(body.len() as u16).to_ne_bytes());
        f.extend_from_slice(body);
        self.s.write_all(&f).unwrap();
    }

    fn recv(&mut self) -> (u16, Vec<u8>) {
        let mut hdr = [0u8; 4];
        self.s.read_exact(&mut hdr).unwrap();
        let opcode = u16::from_ne_bytes([hdr[0], hdr[1]]);
        let size = u16::from_ne_bytes([hdr[2], hdr[3]]) as usize;
        let mut body = vec![0u8; size];
        self.s.read_exact(&mut body).unwrap();
        (opcode, body)
    }
}

fn fd_count(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd")).map(|d| d.count()).unwrap()
}

fn rss_kb(pid: u32) -> usize {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            return v.split_whitespace().next().unwrap().parse().unwrap();
        }
    }
    panic!("no VmRSS");
}

fn wait_fds(child: &Daemon, want: usize) {
    for _ in 0..300 {
        let count = fd_count(child.id());
        assert_ne!(count, 0, "daemon exited while waiting for fd cleanup");
        if count == want {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("fds {} never settled to {want}", fd_count(child.id()));
}

fn open_client(sock: &str) -> Raw {
    let mut c = Raw::connect(sock);
    c.send(1, &[]);
    assert_eq!(c.recv().0, 0x8001);
    c
}

#[test]
fn storm_sigkill_disconnects_and_stale_reclaims() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/storm-sigkill.sock";
    unlink(sock);
    let mut d = spawn_rust(sock);
    let mut a = open_client(sock);
    assert_eq!(a.recv().0, 0x8006);
    d.child.kill().expect("SIGKILL daemon");
    d.child.wait().expect("reap daemon");
    let mut hdr = [0u8; 4];
    let err = a.s.read_exact(&mut hdr).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    drop(a);
    assert!(
        std::path::Path::new(sock).exists(),
        "SIGKILL must leave the socket path behind (no cleanup runs)"
    );
    let d2 = Daemon { child: Command::new(rust_bin())
        .arg("-l")
        .arg("silent")
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .env("SEATD_VTBOUND", "0")
        .env("SEATD_SOCK", sock)
        .spawn()
        .expect("spawn replacement seatd") };
    let mut b = None;
    for _ in 0..50 {
        match UnixStream::connect(sock) {
            Ok(s) => {
                s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
                s.set_write_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
                b = Some(Raw { s });
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
    let mut b = b.expect("replacement seatd never admitted clients");
    b.send(1, &[]);
    assert_eq!(b.recv().0, 0x8001);
    assert_eq!(b.recv().0, 0x8006);
    drop(d2);
    unlink(sock);
}

#[test]
fn storm_disconnect_keeps_other_sessions_switchable() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/storm-disconnect.sock";
    let d = spawn_rust(sock);
    let rest_fds = fd_count(d.id());
    let mut a = open_client(sock);
    assert_eq!(a.recv().0, 0x8006);
    let mut b = open_client(sock);
    let mut c = open_client(sock);
    b.send(2, &[]);
    assert_eq!(b.recv().0, 0x8002);
    drop(b);
    wait_fds(&d, rest_fds + 2);
    a.send(6, &3i32.to_ne_bytes());
    assert_eq!(a.recv().0, 0x8005);
    assert_eq!(a.recv().0, 0x8008);
    a.send(5, &[]);
    assert_eq!(a.recv().0, 0x8009);
    assert_eq!(c.recv().0, 0x8006);
}

#[test]
fn storm_switch_storm_no_growth() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/storm-switch.sock";
    let d = spawn_rust(sock);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let rest_fds = fd_count(d.id());
    let rest_rss = rss_kb(d.id());
    let mut a = open_client(sock);
    assert_eq!(a.recv().0, 0x8006);
    let mut b = open_client(sock);
    let mut c = open_client(sock);
    let clients = [&mut a, &mut b, &mut c];
    let mut active = 0usize;
    for _ in 0..60 {
        let next = (active + 1) % 3;
        let target = (next + 1) as i32;
        clients[active].send(6, &target.to_ne_bytes());
        assert_eq!(clients[active].recv().0, 0x8005, "disable event first");
        assert_eq!(clients[active].recv().0, 0x8008, "then switched");
        clients[active].send(5, &[]);
        assert_eq!(clients[active].recv().0, 0x8009, "disabled ack");
        assert_eq!(clients[next].recv().0, 0x8006, "next enabled");
        active = next;
    }
    drop(a);
    drop(b);
    drop(c);
    wait_fds(&d, rest_fds);
    assert_eq!(fd_count(d.id()), rest_fds, "fd growth after switch storm");
    let rss = rss_kb(d.id());
    println!("storm_switch: rss {rest_rss}kB -> {rss}kB");
    assert!(rss <= rest_rss + 1024, "rss growth after switch storm");
}

#[test]
fn storm_slow_reader_gets_everything() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sock = ".work/tmp/storm-slow.sock";
    let _d = spawn_rust(sock);
    let mut a = open_client(sock);
    assert_eq!(a.recv().0, 0x8006);
    let mut b = open_client(sock);
    a.send(6, &2i32.to_ne_bytes());
    assert_eq!(a.recv().0, 0x8005);
    assert_eq!(a.recv().0, 0x8008);
    a.send(5, &[]);
    assert_eq!(a.recv().0, 0x8009);
    let batch: Vec<u8> = (0..64).flat_map(|_| [7, 0, 0, 0]).collect();
    b.s.write_all(&batch).unwrap();
    a.send(7, &[]);
    assert_eq!(a.recv().0, 0x8007);
    assert_eq!(b.recv().0, 0x8006);
    for _ in 0..64 {
        assert_eq!(b.recv(), (0x8007, vec![]));
    }
    b.send(2, &[]);
    assert_eq!(b.recv().0, 0x8002);
}

fn proc_times(pid: u32) -> (u64, u64, u64) {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let f: Vec<&str> = s.rsplit(')').next().unwrap().split_whitespace().collect();
    assert_ne!(f[0], "Z", "daemon exited during idle measurement");
    let utime: u64 = f[11].parse().unwrap();
    let stime: u64 = f[12].parse().unwrap();
    let st = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let mut vol = 0u64;
    for line in st.lines() {
        if let Some(v) = line.strip_prefix("voluntary_ctxt_switches:") {
            vol = v.trim().parse().unwrap();
        }
    }
    (utime, stime, vol)
}

#[test]
#[ignore = "requires an idle host; bracket with perf-gate-ready and perf-gate-ready -1"]
fn storm_idle_stays_quiet() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rsock = ".work/tmp/storm-idle-rust.sock";
    unlink(rsock);
    let r = spawn_rust(rsock);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (ru0, rs0, rv0) = proc_times(r.id());
    std::thread::sleep(std::time::Duration::from_secs(5));
    let (ru1, rs1, rv1) = proc_times(r.id());
    let r_cpu = (ru1 - ru0) + (rs1 - rs0);
    let r_vol = rv1 - rv0;
    println!("storm_idle: rust cpu={r_cpu} ticks vol={r_vol}");
    assert_eq!(r_cpu, 0, "idle cpu ticks above zero");
    assert!(r_vol <= 10, "egregious idle wakeups");
}
