use seatd::admit;
use seatd::cli;
use seatd::log;
use seatd::r#loop::Server;
use seatd::shutdown;
use seatd::{log_error, log_info};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn os_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn main() {
    let prog = std::env::args().next().unwrap_or_else(|| "seatd".to_string());
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let opts = match cli::parse(&argv, &prog) {
        Ok(o) => o,
        Err(code) => std::process::exit(code),
    };
    if opts.show_help {
        print!("{}", cli::USAGE);
        return;
    }
    if opts.show_version {
        println!("seatd version {VERSION}");
        return;
    }
    log::set_level(opts.level);

    let path = admit::socket_path();
    match admit::path_stat_kind(&path) {
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
        Err(e) => {
            log_error!("Could not stat socket path: {e}");
            std::process::exit(1);
        }
        Ok(mode) => {
            if mode & libc::S_IFMT != libc::S_IFSOCK {
                log_error!(
                    "Non-socket file found at socket path {path}, refusing to start"
                );
                std::process::exit(1);
            } else if !opts.unlink_existing_socket {
                if admit::probe_live(&path) {
                    log_error!("Live daemon owns socket path {path}, refusing to start");
                } else {
                    log_error!("Socket file found at socket path {path}, refusing to start");
                }
                std::process::exit(1);
            } else if admit::probe_live(&path) {
                log_error!("Live daemon owns socket path {path}, refusing to steal it");
                std::process::exit(1);
            } else {
                log_info!("Removing stale socket at {path}");
                if let Err(e) = admit::unlink_path(&path) {
                    log_error!("Could not remove stale socket: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    let mut server = match Server::new() {
        Ok(s) => s,
        Err(e) => {
            log_error!("server_init failed: {e}");
            std::process::exit(1);
        }
    };

    let socket_fd = match admit::open_socket(&path, opts.uid, opts.gid) {
        Ok(fd) => fd,
        Err(e) => {
            log_error!("Could not create server socket: {e}");
            server.finish();
            std::process::exit(1);
        }
    };
    server.set_listen(socket_fd);
    let socket_path = match shutdown::SocketPath::capture(&path) {
        Ok(socket) => socket,
        Err(e) => {
            log_error!("Could not identify bound socket: {e}");
            server.finish();
            std::process::exit(1);
        }
    };

    log_info!("seatd started");

    if opts.readiness != -1 {
        let buf: [u8; 1] = *b"\n";
        let n = unsafe { libc::write(opts.readiness, buf.as_ptr() as *const _, 1) };
        if n != 1 {
            let e = os_error();
            log_error!("Could not write readiness signal: {e}");
        }
        unsafe {
            libc::close(opts.readiness);
        }
    }

    server.run();

    shutdown::shutdown(server, socket_path);
}
