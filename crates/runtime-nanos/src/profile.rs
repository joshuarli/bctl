use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const PROFILE_DISK: &str = "nanos-profile.img";
const INIT_PREFIX: &str = ".nanos-profile-init-";

/// Returns the profile's disk in `dir`, creating it once with `initialize(template, scratch)`.
pub fn prepare_profile(dir: &Path, template: &Path, initialize: impl FnOnce(&Path, &Path) -> io::Result<()>) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let disk = dir.join(PROFILE_DISK);
    match fs::symlink_metadata(&disk) {
        Ok(meta) if meta.file_type().is_file() => return Ok(disk),
        Ok(_) => return Err(format!("{} is not a regular file", disk.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", disk.display())),
    }
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_name().to_string_lossy().starts_with(INIT_PREFIX) {
            return Err(format!("{} contains a profile in a different format", dir.display()));
        }
    }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let scratch = dir.join(format!("{INIT_PREFIX}{}-{nonce}", std::process::id()));
    let result = initialize(template, &scratch)
        .and_then(|()| fs::hard_link(&scratch, &disk));
    let _ = fs::remove_file(&scratch);
    result.map_err(|e| format!("initialize {} from {}: {e}", disk.display(), template.display()))?;
    Ok(disk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(template: &Path, scratch: &Path) -> io::Result<()> {
        fs::copy(template, scratch).map(drop)
    }

    #[test]
    fn profile_initialization_preserves_existing_data_and_rejects_other_formats() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("nanos-profile-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let template = dir.join("template");
        fs::write(&template, b"empty profile").unwrap();
        let profile_dir = dir.join("profile");
        let disk = prepare_profile(&profile_dir, &template, copy).unwrap();
        assert_eq!(fs::read(&disk).unwrap(), b"empty profile");
        fs::write(&disk, b"saved data").unwrap();
        fs::remove_file(&template).unwrap();
        assert_eq!(fs::read(prepare_profile(&profile_dir, &template, copy).unwrap()).unwrap(), b"saved data");
        let other = dir.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("Preferences"), b"existing profile").unwrap();
        assert!(prepare_profile(&other, &template, copy).is_err());
        assert_eq!(fs::read(other.join("Preferences")).unwrap(), b"existing profile");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_initialization_leaves_no_profile_behind() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("nanos-profile-fail-{}", std::process::id()));
        let failing = |_: &Path, scratch: &Path| -> io::Result<()> {
            fs::write(scratch, b"partial")?;
            Err(io::Error::other("convert failed"))
        };
        assert!(prepare_profile(&dir, Path::new("template"), failing).is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
