use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::ConfigError;

pub(crate) fn invalid(path: &str, reason: &str) -> ConfigError {
    ConfigError::InvalidValue {
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

pub(crate) fn duration(path: &str, text: &str) -> Result<Duration, ConfigError> {
    let malformed = || invalid(path, "must be an integer followed by s, m, or h");
    let (index, unit) = text.char_indices().next_back().ok_or_else(malformed)?;
    let (digits, _) = text.split_at(index);
    let seconds_per_unit = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return Err(malformed()),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(malformed());
    }
    let count = digits.parse::<u64>().map_err(|_| invalid(path, "is too large"))?;
    if count == 0 {
        return Err(invalid(path, "must be greater than zero"));
    }
    let seconds = count.checked_mul(seconds_per_unit).ok_or_else(|| invalid(path, "is too large"))?;
    Ok(Duration::from_secs(seconds))
}

pub(crate) fn url(path: &str, text: &str) -> Result<(), ConfigError> {
    let supported = text
        .split_once("://")
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"));
    if supported {
        Ok(())
    } else {
        Err(invalid(path, "must be an http:// or https:// URL"))
    }
}

pub(crate) fn relative_path(path: &str, text: &str) -> Result<PathBuf, ConfigError> {
    let candidate = Path::new(text);
    if candidate.is_absolute() {
        return Err(invalid(path, "must be a relative path"));
    }
    if candidate.components().any(|component| component == Component::ParentDir) {
        return Err(invalid(path, "must not contain '..'"));
    }
    Ok(candidate.to_path_buf())
}
