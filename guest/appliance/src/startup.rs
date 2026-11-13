pub(crate) fn mark(stage: &str) {
    if std::env::var_os("BCTL_STARTUP_TRACE").is_some() {
        let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, &mut time) } == 0 {
            eprintln!("bctl-startup guest {stage} monotonic_us={}", time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1000);
        } else {
            eprintln!("bctl-startup: clock_gettime: {}", std::io::Error::last_os_error());
        }
    }
}
