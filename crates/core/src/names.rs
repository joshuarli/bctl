use std::fmt;

const MACHINE_PREFIX: &str = "bctl-";
const MAX_NAME_LEN: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidName {
    pub name: String,
    pub reason: &'static str,
}

impl fmt::Display for InvalidName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid name {:?}: {}", self.name, self.reason)
    }
}

impl std::error::Error for InvalidName {}

fn validate(name: &str) -> Result<(), InvalidName> {
    let reject = |reason| Err(InvalidName { name: name.to_string(), reason });
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return reject("must be 1 to 40 characters");
    }
    if !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return reject("may contain only a-z, 0-9, and '-'");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return reject("may not start or end with '-'");
    }
    Ok(())
}

macro_rules! name_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(name: &str) -> Result<$name, InvalidName> {
                validate(name)?;
                Ok($name(name.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(SessionName);
name_type!(ProfileName);

/// Incremented every time a session is started, so a VM left over from an earlier start is never mistaken for the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(pub u64);

impl Generation {
    pub fn next(self) -> Generation {
        Generation(self.0 + 1)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The runtime's name for the VM backing one generation of one session: `bctl-<session>-<generation>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MachineName {
    pub session: SessionName,
    pub generation: Generation,
}

impl MachineName {
    /// Parses a runtime machine name; `None` means the machine is not one bctl names.
    pub fn parse(name: &str) -> Option<MachineName> {
        let rest = name.strip_prefix(MACHINE_PREFIX)?;
        let (session, generation) = rest.rsplit_once('-')?;
        if generation.is_empty() || !generation.bytes().all(|b| b.is_ascii_digit()) || generation.starts_with('0') {
            return None;
        }
        Some(MachineName { session: SessionName::new(session).ok()?, generation: Generation(generation.parse().ok()?) })
    }
}

impl fmt::Display for MachineName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{MACHINE_PREFIX}{}-{}", self.session, self.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_names_round_trip_even_when_the_session_has_dashes() {
        let name = MachineName { session: SessionName::new("my-research-2").unwrap(), generation: Generation(17) };
        assert_eq!(name.to_string(), "bctl-my-research-2-17");
        assert_eq!(MachineName::parse("bctl-my-research-2-17"), Some(name));
    }

    #[test]
    fn foreign_and_malformed_machine_names_are_not_bctl_machines() {
        for name in ["vm-0a4a7c5c", "bctl-research", "bctl--3", "bctl-research-", "bctl-research-03", "bctl-Research-3", "bctl-research-x"] {
            assert_eq!(MachineName::parse(name), None, "{name}");
        }
    }

    #[test]
    fn names_reject_characters_the_runtime_or_a_shell_could_misread() {
        for bad in ["", "-a", "a-", "A", "a b", "a/b", "a_b", &"a".repeat(41)] {
            assert!(SessionName::new(bad).is_err(), "{bad:?}");
        }
        assert!(ProfileName::new("personal").is_ok());
    }
}
