use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::layout;
use crate::vsock::{VsockListener, VsockStream};

#[derive(Clone)]
pub struct Log(Arc<Mutex<File>>);

impl Log {
    pub fn open(path: &Path) -> Result<Log, String> {
        let file = OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Log(Arc::new(Mutex::new(file))))
    }

    fn line(&self, text: &str) {
        let mut file = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(error) = writeln!(file, "{text}") {
            eprintln!("bctl-guest: relay log: {error}; dropped: {text}");
        }
    }
}

trait Stream: Read + Write + Send + Sized + 'static {
    fn duplicate(&self) -> io::Result<Self>;
    fn close_write(&self) -> io::Result<()>;
}

impl Stream for UnixStream {
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn close_write(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}

impl Stream for TcpStream {
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn close_write(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
}

impl Stream for VsockStream {
    fn duplicate(&self) -> io::Result<Self> {
        self.try_clone()
    }
    fn close_write(&self) -> io::Result<()> {
        self.shutdown_write()
    }
}

pub fn start_vsock(port: u32, log: &Log) -> Result<(), String> {
    let listener = VsockListener::bind(port).map_err(|e| format!("listen on vsock port {port}: {e}"))?;
    let log = log.clone();
    thread::spawn(move || serve(listener.incoming(), &log));
    Ok(())
}

pub fn start(unix: &Path, tcp_port: Option<u16>, log: &Log) -> Result<(), String> {
    match fs::remove_file(unix) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", unix.display())),
    }
    let listener = UnixListener::bind(unix).map_err(|e| format!("listen on {}: {e}", unix.display()))?;
    let unix_log = log.clone();
    thread::spawn(move || serve(listener.incoming(), &unix_log));
    if let Some(port) = tcp_port {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).map_err(|e| format!("listen on TCP {port}: {e}"))?;
        let tcp_log = log.clone();
        thread::spawn(move || serve(listener.incoming(), &tcp_log));
    }
    Ok(())
}

fn serve<S: Stream>(incoming: impl Iterator<Item = io::Result<S>>, log: &Log) {
    for client in incoming {
        match client {
            Ok(client) => {
                let log = log.clone();
                thread::spawn(move || bridge(client, &log));
            }
            Err(error) => log.line(&format!("accept: {error}")),
        }
    }
}

fn bridge<S: Stream>(client: S, log: &Log) {
    let upstream = match TcpStream::connect((Ipv4Addr::LOCALHOST, layout::BROWSER_CDP_PORT)) {
        Ok(upstream) => upstream,
        Err(error) => return log.line(&format!("connect to the browser: {error}")),
    };
    let (mut client_reader, mut upstream_writer) = match (client.duplicate(), upstream.try_clone()) {
        (Ok(reader), Ok(writer)) => (reader, writer),
        (Err(error), _) | (_, Err(error)) => return log.line(&format!("duplicate a relay stream: {error}")),
    };
    let forward = thread::spawn(move || {
        let copied = io::copy(&mut client_reader, &mut upstream_writer);
        let closed = upstream_writer.shutdown(Shutdown::Write);
        copied.and(closed)
    });
    let (mut upstream_reader, mut client_writer) = (upstream, client);
    let back = io::copy(&mut upstream_reader, &mut client_writer).and_then(|_| client_writer.close_write());
    let forward = forward.join().unwrap_or_else(|_| Err(io::Error::other("relay thread panicked")));
    for (direction, result) in [("to the browser", forward.map(|_| ())), ("from the browser", back)] {
        if let Err(error) = result
            && !matches!(error.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected)
        {
            log.line(&format!("relay {direction}: {error}"));
        }
    }
}
