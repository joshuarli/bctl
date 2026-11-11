mod accounts;
mod browser;
mod cdp;
mod check;
mod devices;
mod headless;
mod layout;
mod maps;
mod procfs;
mod relay;
mod signals;
mod supervise;
mod vsock;

use std::process::ExitCode;

const USAGE: &str = "usage: bctl-guest [run|run-headless|run-headless-persistent|check|browser|maps]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["run"] => supervise::run(),
        ["run-headless"] => headless::run(false),
        ["run-headless-persistent"] => headless::run(true),
        ["check"] => check::run(),
        ["browser"] => browser::exec(),
        ["maps"] => maps::print(),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("bctl-guest: {message}");
            ExitCode::FAILURE
        }
    }
}
