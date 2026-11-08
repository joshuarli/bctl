use std::process::Command;

#[test]
fn malformed_options_return_usage_error_without_panicking() {
    for args in [vec!["--width"], vec!["--height"], vec!["--drm-device"], vec!["--socket"],
        vec!["--width", "invalid"], vec!["--height", "-1"], vec!["--unknown-flag"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_tinyland")).args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "args={args:?}, stderr={}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage: tinyland"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
}
