use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

pub fn user(name: &str) -> Result<Account, String> {
    let passwd = read("/etc/passwd")?;
    find_user(&passwd, name).ok_or_else(|| format!("no user {name} in /etc/passwd"))
}

pub fn group(name: &str) -> Result<u32, String> {
    let group = read("/etc/group")?;
    find_group(&group, name).ok_or_else(|| format!("no group {name} in /etc/group"))
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn find_user(passwd: &str, name: &str) -> Option<Account> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        match fields.as_slice() {
            [user, _, uid, gid, _, home, ..] if *user == name => {
                Some(Account { uid: uid.parse().ok()?, gid: gid.parse().ok()?, home: PathBuf::from(home) })
            }
            _ => None,
        }
    })
}

fn find_group(group: &str, name: &str) -> Option<u32> {
    group.lines().find_map(|line| match line.split(':').collect::<Vec<_>>().as_slice() {
        [group, _, gid, ..] if *group == name => gid.parse().ok(),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_accounts_name_the_browser_user_and_device_group() {
        let passwd = include_str!("../rootfs/etc/passwd");
        let group = include_str!("../rootfs/etc/group");
        assert_eq!(
            find_user(passwd, "browser"),
            Some(Account { uid: 1000, gid: 27, home: PathBuf::from("/home/browser") })
        );
        assert_eq!(find_group(group, "video"), Some(27));
    }

    #[test]
    fn a_missing_or_malformed_entry_is_not_found() {
        assert_eq!(find_user("browser:x:1000\nother:x:1:1::/:/x\n", "browser"), None);
        assert_eq!(find_user("browser:x:uid:27::/home/browser:/x\n", "browser"), None);
        assert_eq!(find_group("videos:x:28:\n", "video"), None);
    }
}
