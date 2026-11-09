use std::fs;

pub fn pids() -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else { return Vec::new() };
    let mut pids: Vec<u32> = entries.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect();
    pids.sort_unstable();
    pids
}

struct Stat {
    state: char,
    parent: u32,
}

fn parse_stat(stat: &str) -> Option<Stat> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_comm.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?.parse().ok()?;
    Some(Stat { state, parent })
}

fn stat(pid: u32) -> Option<Stat> {
    parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

pub fn children(parent: u32) -> Vec<u32> {
    pids().into_iter().filter(|&pid| stat(pid).is_some_and(|s| s.parent == parent)).collect()
}

pub fn alive(pid: u32) -> bool {
    stat(pid).is_some_and(|s| !matches!(s.state, 'Z' | 'X' | 'x'))
}

pub fn cmdline(pid: u32) -> Vec<String> {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|raw| raw.split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
        .unwrap_or_default()
}

pub fn status(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/status")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_are_read_after_the_last_parenthesis_of_the_command_name() {
        let stat = parse_stat("4242 (bctl (guest)) S 17 4242 4242 0 -1 4194560").unwrap();
        assert_eq!((stat.state, stat.parent), ('S', 17));
        assert!(parse_stat("4242 (truncated").is_none());
    }
}
