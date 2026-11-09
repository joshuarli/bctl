use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const CARD: &str = "/dev/dri/card0";

pub fn expose(group: u32) -> Result<(), String> {
    fs::create_dir_all("/dev/input").map_err(|e| format!("/dev/input: {e}"))?;
    for entry in fs::read_dir("/sys/class/input").map_err(|e| format!("/sys/class/input: {e}"))? {
        let entry = entry.map_err(|e| format!("/sys/class/input: {e}"))?;
        let name = entry.file_name();
        if !name.as_bytes().starts_with(b"event") {
            continue;
        }
        let number = fs::read_to_string(entry.path().join("dev")).map_err(|e| format!("{}: {e}", entry.path().display()))?;
        let (major, minor) = parse_device_number(&number).ok_or_else(|| format!("{}: bad device number {number:?}", entry.path().display()))?;
        let node = Path::new("/dev/input").join(&name);
        if !node.exists() {
            make_char_device(&node, major, minor)?;
        }
    }
    if !Path::new(CARD).exists() {
        return Err(format!("no {CARD}: start the machine with a display"));
    }
    for directory in ["/dev/dri", "/dev/input"] {
        for entry in fs::read_dir(directory).map_err(|e| format!("{directory}: {e}"))? {
            let path = entry.map_err(|e| format!("{directory}: {e}"))?.path();
            std::os::unix::fs::chown(&path, None, Some(group)).map_err(|e| format!("chgrp {}: {e}", path.display()))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).map_err(|e| format!("chmod {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

fn parse_device_number(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.trim().split_once(':')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn make_char_device(path: &Path, major: u32, minor: u32) -> Result<(), String> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let result = unsafe { libc::mknod(c_path.as_ptr(), libc::S_IFCHR | 0o660, libc::makedev(major, minor)) };
    if result != 0 {
        return Err(format!("mknod {}: {}", path.display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_numbers_are_major_colon_minor() {
        assert_eq!(parse_device_number("13:65\n"), Some((13, 65)));
        assert_eq!(parse_device_number("13"), None);
        assert_eq!(parse_device_number("a:1"), None);
    }
}
