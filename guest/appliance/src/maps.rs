use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use crate::{layout, procfs};

const SAMPLE_EVERY: Duration = Duration::from_millis(300);

fn table() -> PathBuf {
    layout::log("maps.tsv")
}

pub fn requested() -> Result<bool, String> {
    match std::env::var("BCTL_TRACE_MAPS") {
        Err(std::env::VarError::NotPresent) => Ok(false),
        Ok(value) if value == "1" => Ok(true),
        Ok(value) => Err(format!("BCTL_TRACE_MAPS must be 1 or unset, not {value:?}")),
        Err(std::env::VarError::NotUnicode(value)) => Err(format!("BCTL_TRACE_MAPS must be 1 or unset, not {value:?}")),
    }
}

pub fn start() {
    thread::spawn(|| {
        let mut seen = BTreeSet::new();
        loop {
            let before = seen.len();
            for pid in procfs::pids() {
                if let Ok(maps) = fs::read_to_string(format!("/proc/{pid}/maps")) {
                    seen.extend(mappings(&maps));
                }
            }
            if seen.len() != before
                && let Err(error) = write(&seen)
            {
                eprintln!("bctl-guest: maps trace: {error}");
            }
            thread::sleep(SAMPLE_EVERY);
        }
    });
}

fn write(seen: &BTreeSet<(String, String)>) -> std::io::Result<()> {
    let partial = table().with_extension("tsv.part");
    let mut file = fs::File::create(&partial)?;
    for (executable, mapped) in seen {
        writeln!(file, "{executable}\t{mapped}")?;
    }
    fs::rename(partial, table())
}

pub fn print() -> Result<ExitCode, String> {
    let text = fs::read_to_string(table()).map_err(|e| format!("{}: {e}", table().display()))?;
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

fn mappings(maps: &str) -> Vec<(String, String)> {
    let paths: Vec<&str> = maps.lines().filter_map(mapped_path).collect();
    let Some(executable) = paths.first() else { return Vec::new() };
    paths.iter().map(|path| (executable.to_string(), path.to_string())).collect()
}

fn mapped_path(line: &str) -> Option<&str> {
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start().split_once(' ')?.1;
    }
    let path = rest.trim_start();
    path.starts_with('/').then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mapped_file_is_paired_with_the_first_mapped_file_of_the_process() {
        let maps = "\
aaaab0000000-aaaab0100000 r--p 00000000 00:1f 123   /opt/browser/chromium
aaaab0100000-aaaab0200000 rw-p 00000000 00:00 0 \n\
ffff80000000-ffff80100000 r-xp 00000000 00:1f 456   /opt/browser/lib/libz.so.1
ffff80200000-ffff80300000 r--s 00000000 00:05 7     /dev/shm/.org.chromium.Chromium.x (deleted)
ffff90000000-ffff90021000 rw-p 00000000 00:00 0     [stack]
";
        assert_eq!(
            mappings(maps),
            [
                ("/opt/browser/chromium", "/opt/browser/chromium"),
                ("/opt/browser/chromium", "/opt/browser/lib/libz.so.1"),
                ("/opt/browser/chromium", "/dev/shm/.org.chromium.Chromium.x (deleted)"),
            ]
            .map(|(e, p)| (e.to_string(), p.to_string()))
        );
        assert!(mappings("ffff90000000-ffff90021000 rw-p 00000000 00:00 0 [heap]\n").is_empty());
    }
}
