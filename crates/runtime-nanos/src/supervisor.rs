use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::egress::{self, Policy};
use crate::{CONSOLE_LOG, CONTROL_SOCKET, GUEST_CDP_PORT, GUEST_EGRESS_PORT, SUPERVISOR_LOG, VSOCK_SOCKET, connect_guest};

const USAGE: &str = "usage: bctl-nanos-vm launch|serve --dir DIR --image IMAGE --cdp-socket PATH --memory-mib N --cpus N --qemu PATH --qemu-img PATH --kernel PATH --vsock-backend PATH [--allow-private CIDR]...";
const VHOST_SOCKET: &str = "vhost.sock";
const QMP_SOCKET: &str = "qmp.sock";
const DISK: &str = "disk.qcow2";
const GUEST_CID: u32 = 3;
const START_TIMEOUT: Duration = Duration::from_secs(30);
const QUIT_GRACE: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
struct Config {
    dir: PathBuf,
    image: PathBuf,
    cdp_socket: PathBuf,
    memory_mib: u32,
    cpus: u8,
    qemu: PathBuf,
    qemu_img: PathBuf,
    kernel: PathBuf,
    vsock_backend: PathBuf,
    allow_private: Vec<String>,
}

impl Config {
    fn parse(args: &[String]) -> Result<Config, String> {
        let value = |name: &str| -> Result<String, String> {
            let at = args.iter().position(|a| a == name).ok_or_else(|| format!("missing {name}"))?;
            args.get(at + 1).cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        let path = |name: &str| value(name).map(PathBuf::from);
        Ok(Config {
            dir: path("--dir")?,
            image: path("--image")?,
            cdp_socket: path("--cdp-socket")?,
            memory_mib: value("--memory-mib")?.parse().map_err(|_| "--memory-mib is not a number")?,
            cpus: value("--cpus")?.parse().map_err(|_| "--cpus is not a number")?,
            qemu: path("--qemu")?,
            qemu_img: path("--qemu-img")?,
            kernel: path("--kernel")?,
            vsock_backend: path("--vsock-backend")?,
            allow_private: args.windows(2).filter(|pair| pair[0] == "--allow-private").map(|pair| pair[1].clone()).collect(),
        })
    }
}

/// Runs the supervisor with `args` (`launch ...` or `serve ...`); `prefix` is what the executable needs before
/// those to reach this function again, so `launch` can start `serve` from the same binary.
pub fn main(prefix: &[&str], args: &[String]) -> ExitCode {
    let result = match args.split_first() {
        Some((command, rest)) if command == "launch" => Config::parse(rest).and_then(|config| launch(prefix, &config, rest)),
        Some((command, rest)) if command == "serve" => Config::parse(rest).and_then(|config| serve(&config)),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("bctl-nanos-vm: {message}");
            ExitCode::FAILURE
        }
    }
}

fn io_error(what: impl AsRef<Path>) -> impl FnOnce(io::Error) -> String {
    move |error| format!("{}: {error}", what.as_ref().display())
}

fn log_file(dir: &Path, name: &str) -> Result<File, String> {
    let path = dir.join(name);
    OpenOptions::new().create(true).append(true).open(&path).map_err(io_error(&path))
}

fn launch(prefix: &[&str], config: &Config, args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(io_error("current executable"))?;
    let mut serve = Command::new(&exe)
        .args(prefix)
        .arg("serve")
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log_file(&config.dir, SUPERVISOR_LOG)?)
        .spawn()
        .map_err(io_error(&exe))?;
    let stdout = serve.stdout.take().expect("stdout was piped");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    match receiver.recv_timeout(START_TIMEOUT) {
        Ok(line) if line.trim() == "ready" => Ok(()),
        outcome => {
            let _ = serve.kill();
            let _ = serve.wait();
            let log = fs::read_to_string(config.dir.join(SUPERVISOR_LOG)).unwrap_or_default();
            let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message");
            Err(match outcome {
                Ok(_) => format!("the supervisor stopped before QEMU ran: {last}"),
                Err(_) => format!("the supervisor did not report QEMU running within {START_TIMEOUT:?}: {last}"),
            })
        }
    }
}

fn wait_for(path: &Path, child: &mut Child, what: &str) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    while !path.exists() {
        if let Some(status) = child.try_wait().map_err(io_error(what))? {
            return Err(format!("{what} exited before creating {}: {status}", path.display()));
        }
        if Instant::now() >= deadline {
            return Err(format!("{what} did not create {} within {START_TIMEOUT:?}", path.display()));
        }
        thread::sleep(TICK);
    }
    Ok(())
}

fn remove_stale(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn qemu_command(config: &Config) -> Result<Command, String> {
    let dir = &config.dir;
    let serial = dir.join(CONSOLE_LOG);
    let mut command = Command::new(&config.qemu);
    command
        .args(["-machine", "q35,accel=kvm,memory-backend=mem", "-cpu", "host", "-nodefaults", "-no-user-config"])
        .args(["-object", &format!("memory-backend-memfd,id=mem,size={}M,share=on", config.memory_mib)])
        .args(["-m", &format!("{}M", config.memory_mib), "-smp", &config.cpus.to_string()])
        .args(["-display", "none", "-monitor", "none", "-no-reboot"])
        .args(["-serial", &format!("file:{}", serial.display())])
        .args(["-qmp", &format!("unix:{},server=on,wait=off", dir.join(QMP_SOCKET).display())])
        .args(["-device", "isa-debug-exit", "-device", "virtio-rng-pci"])
        .args(["-device", "virtio-balloon-pci,free-page-reporting=on,vectors=5"])
        .args(["-drive", &format!("if=none,id=hd0,format=qcow2,file={}", dir.join(DISK).display())])
        .args(["-device", "virtio-scsi-pci,id=scsi0", "-device", "scsi-hd,bus=scsi0.0,drive=hd0"])
        .args(["-nic", "none"])
        .args(["-chardev", &format!("socket,id=vsock,path={}", dir.join(VHOST_SOCKET).display())])
        .args(["-device", "vhost-user-vsock-pci,chardev=vsock"])
        .arg("-kernel")
        .arg(&config.kernel)
        .stdin(Stdio::null())
        .stdout(log_file(dir, "qemu.log")?)
        .stderr(log_file(dir, "qemu.log")?);
    Ok(command)
}

fn serve(config: &Config) -> Result<(), String> {
    let dir = &config.dir;
    let image = fs::canonicalize(&config.image).map_err(io_error(&config.image))?;
    let disk = dir.join(DISK);
    let created = Command::new(&config.qemu_img)
        .args(["create", "-q", "-f", "qcow2", "-F", "raw", "-b"])
        .arg(&image)
        .arg(&disk)
        .output()
        .map_err(io_error(&config.qemu_img))?;
    if !created.status.success() {
        return Err(format!("qemu-img create: {}", String::from_utf8_lossy(&created.stderr).trim()));
    }

    let vhost = dir.join(VHOST_SOCKET);
    let vsock = dir.join(VSOCK_SOCKET);
    let mut backend = Command::new(&config.vsock_backend)
        .arg("--vm")
        .arg(format!("guest-cid={GUEST_CID},socket={},uds-path={}", vhost.display(), vsock.display()))
        .stdin(Stdio::null())
        .stdout(log_file(dir, "vsock.log")?)
        .stderr(log_file(dir, "vsock.log")?)
        .spawn()
        .map_err(io_error(&config.vsock_backend))?;
    if let Err(error) = wait_for(&vhost, &mut backend, "vhost-device-vsock") {
        let _ = backend.kill();
        return Err(error);
    }

    let mut qemu = match qemu_command(config)?.spawn() {
        Ok(qemu) => qemu,
        Err(error) => {
            let _ = backend.kill();
            return Err(format!("{}: {error}", config.qemu.display()));
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let started = wait_for(&dir.join(QMP_SOCKET), &mut qemu, "qemu")
        .and_then(|()| listen(config, &vsock, Arc::clone(&stop)))
        .and_then(|()| {
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "ready").and_then(|()| stdout.flush()).map_err(io_error("stdout"))
        });
    if let Err(error) = started {
        let _ = qemu.kill();
        let _ = qemu.wait();
        let _ = backend.kill();
        let _ = backend.wait();
        cleanup(config);
        return Err(error);
    }

    let mut quit_sent_at: Option<Instant> = None;
    let status = loop {
        if let Some(status) = qemu.try_wait().map_err(io_error("qemu"))? {
            break status;
        }
        if stop.load(Ordering::SeqCst) {
            match quit_sent_at {
                None => {
                    if let Err(error) = qmp_quit(&dir.join(QMP_SOCKET)) {
                        eprintln!("bctl-nanos-vm: QMP quit: {error}; killing qemu");
                        let _ = qemu.kill();
                    }
                    quit_sent_at = Some(Instant::now());
                }
                Some(at) if at.elapsed() > QUIT_GRACE => {
                    eprintln!("bctl-nanos-vm: qemu ignored quit for {QUIT_GRACE:?}; killing it");
                    let _ = qemu.kill();
                }
                Some(_) => {}
            }
        }
        thread::sleep(TICK);
    };
    eprintln!("bctl-nanos-vm: qemu exited: {status}");
    let _ = backend.kill();
    let _ = backend.wait();
    cleanup(config);
    Ok(())
}

fn egress_socket(dir: &Path) -> PathBuf {
    dir.join(format!("{VSOCK_SOCKET}_{GUEST_EGRESS_PORT}"))
}

fn cleanup(config: &Config) {
    for path in [
        config.dir.join(CONTROL_SOCKET),
        config.cdp_socket.clone(),
        config.dir.join(VHOST_SOCKET),
        config.dir.join(VSOCK_SOCKET),
        egress_socket(&config.dir),
    ] {
        let _ = fs::remove_file(path);
    }
}

fn listen(config: &Config, vsock: &Path, stop: Arc<AtomicBool>) -> Result<(), String> {
    let control_path = config.dir.join(CONTROL_SOCKET);
    remove_stale(&control_path)?;
    let control = UnixListener::bind(&control_path).map_err(io_error(&control_path))?;
    remove_stale(&config.cdp_socket)?;
    let cdp = UnixListener::bind(&config.cdp_socket).map_err(io_error(&config.cdp_socket))?;
    let policy = Policy::new(&config.allow_private)?;
    let egress_path = egress_socket(&config.dir);
    remove_stale(&egress_path)?;
    let egress = UnixListener::bind(&egress_path).map_err(io_error(&egress_path))?;
    thread::spawn(move || {
        for client in egress.incoming() {
            match client {
                Ok(client) => {
                    let policy = policy.clone();
                    thread::spawn(move || egress::serve(client, &policy));
                }
                Err(error) => eprintln!("bctl-nanos-vm: egress accept: {error}"),
            }
        }
    });
    thread::spawn(move || {
        for client in control.incoming().flatten() {
            serve_control(client, &stop);
        }
    });
    let vsock = vsock.to_path_buf();
    thread::spawn(move || {
        for client in cdp.incoming() {
            match client {
                Ok(client) => {
                    let vsock = vsock.clone();
                    thread::spawn(move || relay(client, &vsock));
                }
                Err(error) => eprintln!("bctl-nanos-vm: CDP accept: {error}"),
            }
        }
    });
    Ok(())
}

fn serve_control(client: UnixStream, stop: &AtomicBool) {
    let mut line = String::new();
    if BufReader::new(&client).read_line(&mut line).is_err() {
        return;
    }
    let reply = match line.trim() {
        "status" if stop.load(Ordering::SeqCst) => "stopping",
        "status" => "running",
        "stop" => {
            stop.store(true, Ordering::SeqCst);
            "stopping"
        }
        _ => "unknown command",
    };
    let _ = (&client).write_all(format!("{reply}\n").as_bytes());
}

fn relay(client: UnixStream, vsock: &Path) {
    let upstream = match connect_guest(vsock, GUEST_CDP_PORT, Duration::from_secs(5)) {
        Ok(upstream) => upstream,
        Err(error) => {
            eprintln!("bctl-nanos-vm: connect to the guest CDP port: {error}");
            return;
        }
    };
    let (Ok(mut client_reader), Ok(mut upstream_writer)) = (client.try_clone(), upstream.try_clone()) else {
        eprintln!("bctl-nanos-vm: duplicate a relay stream");
        return;
    };
    let forward = thread::spawn(move || {
        let _ = io::copy(&mut client_reader, &mut upstream_writer);
        let _ = upstream_writer.shutdown(Shutdown::Write);
    });
    let (mut upstream_reader, mut client_writer) = (upstream, client);
    let _ = io::copy(&mut upstream_reader, &mut client_writer);
    let _ = client_writer.shutdown(Shutdown::Write);
    let _ = forward.join();
}

fn qmp_quit(socket: &Path) -> io::Result<()> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    (&stream).write_all(b"{\"execute\":\"qmp_capabilities\"}\n")?;
    line.clear();
    reader.read_line(&mut line)?;
    (&stream).write_all(b"{\"execute\":\"quit\"}\n")
}
