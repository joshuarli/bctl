use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Silent = 0,
    Error = 1,
    Info = 2,
    Debug = 3,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn emit(level: Level, file: &str, line: u32, msg: std::fmt::Arguments) {
    if level as u8 <= LEVEL.load(Ordering::Relaxed) && level != Level::Silent {
        let tag = match level {
            Level::Error => "error",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Silent => "silent",
        };
        eprintln!("[{tag}] [{file}:{line}] {msg}");
    }
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Error, file!(), line!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Info, file!(), line!(), format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::log::emit($crate::log::Level::Debug, file!(), line!(), format_args!($($arg)*))
    };
}
