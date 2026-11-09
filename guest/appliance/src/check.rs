use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use crate::{accounts, layout, procfs};

const VNC_INPUTS: [&str; 2] = ["smolvm vnc pointer", "smolvm vnc keyboard"];

pub fn run() -> Result<ExitCode, String> {
    let browser = accounts::user(layout::BROWSER_USER)?;
    let mut complete = true;
    for (stage, ok) in [
        ("seat", is_socket(Path::new(layout::SEAT_SOCKET))),
        ("compositor", is_socket(&layout::wayland_socket(browser.uid))),
        ("browser", cdp_answers()),
        ("sandbox", renderer_sandboxed()),
    ] {
        println!("{stage} {}", if ok { "ok" } else { "missing" });
        complete &= ok;
    }
    println!("input {}", if vnc_input_present(&input_names()) { "ok" } else { "absent" });
    Ok(if complete { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

pub fn is_socket(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

pub fn cdp_answers() -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, layout::BROWSER_CDP_PORT));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_secs(1)) else { return false };
    if stream.set_read_timeout(Some(Duration::from_secs(2))).is_err() {
        return false;
    }
    let request = format!("GET /json/version HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).is_ok() && head.starts_with(b"HTTP/1.1 200")
}

fn renderer_sandboxed() -> bool {
    procfs::pids()
        .into_iter()
        .find(|&pid| is_renderer(&procfs::cmdline(pid)))
        .and_then(procfs::status)
        .is_some_and(|status| sandboxed(&status))
}

fn is_renderer(cmdline: &[String]) -> bool {
    cmdline.iter().flat_map(|arg| arg.split(' ')).any(|word| word == "--type=renderer")
}

fn sandboxed(status: &str) -> bool {
    let field = |name: &str| {
        status.lines().find_map(|line| line.split_once(':').filter(|(key, _)| *key == name).map(|(_, value)| value.trim()))
    };
    field("NoNewPrivs") == Some("1") && field("Seccomp") == Some("2")
}

fn input_names() -> Vec<String> {
    let Ok(entries) = fs::read_dir("/sys/class/input") else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("input"))
        .filter_map(|e| fs::read_to_string(e.path().join("name")).ok())
        .map(|name| name.trim_end_matches('\n').to_string())
        .collect()
}

fn vnc_input_present(names: &[String]) -> bool {
    VNC_INPUTS.iter().all(|wanted| names.iter().any(|name| name == wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RENDERER: &str = "Name:\tchromium\nNoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t3\n";

    #[test]
    fn a_renderer_is_sandboxed_only_with_no_new_privs_and_a_seccomp_filter() {
        assert!(sandboxed(RENDERER));
        assert!(!sandboxed(&RENDERER.replace("NoNewPrivs:\t1", "NoNewPrivs:\t0")));
        assert!(!sandboxed(&RENDERER.replace("Seccomp:\t2", "Seccomp:\t0")));
        assert!(!sandboxed("Name:\tchromium\nSeccomp_filters:\t2\n"));
    }

    #[test]
    fn a_renderer_is_found_whether_or_not_chromium_rewrote_its_command_line() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(is_renderer(&args(&["/opt/browser/chromium", "--type=renderer", "--lang=en-US"])));
        assert!(is_renderer(&args(&["/opt/browser/chromium --type=renderer --crashpad-handler-pid=0"])));
        assert!(!is_renderer(&args(&["/opt/browser/chromium --type=gpu-process --renderer-client-id=4"])));
    }

    #[test]
    fn input_is_present_only_when_both_vnc_devices_exist() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(vnc_input_present(&names(&["smolvm vnc keyboard", "Power Button", "smolvm vnc pointer"])));
        assert!(!vnc_input_present(&names(&["smolvm vnc keyboard"])));
        assert!(!vnc_input_present(&names(&["smolvm vnc pointer2", "smolvm vnc keyboard"])));
    }
}
