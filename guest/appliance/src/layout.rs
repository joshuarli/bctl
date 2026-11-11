use std::path::{Path, PathBuf};

pub const STATE_DIR: &str = "/run/bctl";
pub const SEAT_SOCKET: &str = "/run/seatd.sock";
pub const WAYLAND_SOCKET: &str = "wayland-1";
pub const BROWSER_CDP_PORT: u16 = 9221;
pub const VSOCK_CDP_PORT: u32 = 9222;
pub const VSOCK_CHECK_PORT: u32 = 9223;
pub const VSOCK_STOP_PORT: u32 = 9224;
pub const VSOCK_EGRESS_PORT: u32 = 1080;
pub const EGRESS_PROXY_PORT: u16 = 1080;
pub const VSOCK_HOST_CID: u32 = 2;
pub const BROWSER_USER: &str = "browser";
pub const DEVICE_GROUP: &str = "video";
pub const BROWSER_LINK: &str = "/usr/local/bin/browser";
pub const SELF: &str = "/usr/local/bin/bctl-guest";
pub const SEATD: &str = "/usr/local/bin/seatd";
pub const TINYLAND: &str = "/usr/local/bin/tinyland";

pub fn runtime_dir(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}"))
}

pub fn wayland_socket(uid: u32) -> PathBuf {
    runtime_dir(uid).join(WAYLAND_SOCKET)
}

pub fn log(name: &str) -> PathBuf {
    PathBuf::from(STATE_DIR).join(name)
}

fn setting(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

pub fn profile_dir() -> PathBuf {
    PathBuf::from(setting("BCTL_PROFILE_DIR", "/profile"))
}

/// The browser's default download directory, where the host's runtime may mount a session's files directory.
pub fn downloads_dir(home: &Path) -> PathBuf {
    home.join("Downloads")
}

pub fn cdp_socket() -> PathBuf {
    PathBuf::from(setting("BCTL_CDP_SOCKET", "/run/bctl/cdp.sock"))
}

pub fn start_url() -> String {
    setting("BCTL_START_URL", "about:blank")
}

pub fn cdp_tcp_port() -> Result<Option<u16>, String> {
    match std::env::var("BCTL_CDP_PORT") {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => value.parse().map(Some).map_err(|_| format!("BCTL_CDP_PORT {value:?} is not a port")),
        Err(std::env::VarError::NotUnicode(value)) => Err(format!("BCTL_CDP_PORT {value:?} is not a port")),
    }
}
