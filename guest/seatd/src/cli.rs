use crate::log::Level;
use std::ffi::CString;

pub const USAGE: &str = "Usage: seatd [options]\n\
    \n\
    \t-h\t\tShow this help message\n\
    \t-n <fd>\tFD to notify readiness on\n\
    \t-u <user>\tUser to own the seatd socket\n\
    \t-g <group>\tGroup to own the seatd socket\n\
    \t-l <loglevel>\tLog-level, one of debug, info, error or silent\n\
    \t-v\t\tShow the version number\n\
    \n";

pub struct Options {
    pub uid: i64,
    pub gid: i64,
    pub readiness: i32,
    pub unlink_existing_socket: bool,
    pub level: Level,
    pub show_version: bool,
    pub show_help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            uid: -1,
            gid: -1,
            readiness: -1,
            unlink_existing_socket: true,
            level: Level::Info,
            show_version: false,
            show_help: false,
        }
    }
}

fn lookup_user(name: &str) -> Option<i64> {
    let c = CString::new(name).ok()?;
    unsafe {
        let pw = libc::getpwnam(c.as_ptr());
        if pw.is_null() {
            None
        } else {
            Some((*pw).pw_uid as i64)
        }
    }
}

fn lookup_group(name: &str) -> Option<i64> {
    let c = CString::new(name).ok()?;
    unsafe {
        let gr = libc::getgrnam(c.as_ptr());
        if gr.is_null() {
            None
        } else {
            Some((*gr).gr_gid as i64)
        }
    }
}

pub fn parse(argv: &[String], prog: &str) -> Result<Options, i32> {
    let mut opts = Options::default();
    let mut args = argv.iter().peekable();
    let mut end_of_opts = false;
    while let Some(arg) = args.next() {
        let bytes = arg.as_bytes();
        if end_of_opts || bytes.len() < 2 || bytes[0] != b'-' {
            continue;
        }
        if arg == "--" {
            end_of_opts = true;
            continue;
        }
        let mut chars = bytes[1..].iter();
        while let Some(&c) = chars.next() {
            let needs_arg = matches!(c, b'n' | b'u' | b'g' | b'l');
            let optarg: Option<String> = if needs_arg {
                let rest: Vec<u8> = chars.by_ref().copied().collect();
                if !rest.is_empty() {
                    Some(String::from_utf8_lossy(&rest).into_owned())
                } else {
                    match args.next() {
                        Some(a) => Some(a.clone()),
                        None => {
                            eprintln!("Try '{prog} -h' for more information.");
                            return Err(1);
                        }
                    }
                }
            } else {
                None
            };
            match c {
                b'n' => {
                    let s = optarg.unwrap();
                    match s.parse::<i32>() {
                        Ok(v) if v >= 0 => opts.readiness = v,
                        _ => {
                            eprintln!("Invalid readiness fd: {s}");
                            return Err(1);
                        }
                    }
                }
                b'u' => {
                    if !opts.unlink_existing_socket {
                        eprintln!("-u/-g and -z are mutually exclusive");
                        return Err(1);
                    }
                    let s = optarg.unwrap();
                    match lookup_user(&s) {
                        Some(uid) => opts.uid = uid,
                        None => {
                            eprintln!("Could not find user by name '{s}'.");
                            return Err(1);
                        }
                    }
                }
                b'g' => {
                    if !opts.unlink_existing_socket {
                        eprintln!("-u/-g and -z are mutually exclusive");
                        return Err(1);
                    }
                    let s = optarg.unwrap();
                    match lookup_group(&s) {
                        Some(gid) => opts.gid = gid,
                        None => {
                            eprintln!("Could not find group by name '{s}'.");
                            return Err(1);
                        }
                    }
                }
                b'l' => {
                    let s = optarg.unwrap();
                    match s.as_str() {
                        "debug" => opts.level = Level::Debug,
                        "info" => opts.level = Level::Info,
                        "error" => opts.level = Level::Error,
                        "silent" => opts.level = Level::Silent,
                        _ => {
                            eprintln!("Invalid loglevel: {s}");
                            return Err(1);
                        }
                    }
                }
                b'z' => {
                    if opts.uid != -1 || opts.gid != -1 {
                        eprintln!("-u/-g and -z are mutually exclusive");
                        return Err(1);
                    }
                    opts.unlink_existing_socket = false;
                }
                b'v' => opts.show_version = true,
                b'h' => opts.show_help = true,
                _ => {
                    eprintln!("Try '{prog} -h' for more information.");
                    return Err(1);
                }
            }
            if needs_arg {
                break;
            }
        }
    }
    Ok(opts)
}
