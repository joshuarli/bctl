fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    bctl_runtime_nanos::supervisor::main(&[], &args)
}
