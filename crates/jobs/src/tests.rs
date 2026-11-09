use std::path::PathBuf;
use std::time::Duration;

use super::*;

const FULL: &str = r#"
version = 1

[browsers.default]
image = "ref-alpine-chromium"

[browsers.wide]
image = "ref-alpine-chromium"
viewport = [1920, 1080]
cpus = 8
memory_mib = 8192

[jobs.mirror-docs]
kind = "grab"
browser = "default"
profile = "docs"
urls = ["https://example.com/docs/", "http://example.com/more"]
max_pages = 500
jobs = 4
robots = false
max_runtime = "30m"

[jobs.extract-products]
kind = "exec"
browser = "wide"
argv = ["python3", "jobs/extract.py"]
cwd = "jobs"
timeout = "10m"
"#;

fn config(source: &str) -> Config {
    parse(source).unwrap_or_else(|error| panic!("expected a valid config, got {error}"))
}

fn error(source: &str) -> ConfigError {
    match parse(source) {
        Ok(_) => panic!("expected a rejection"),
        Err(error) => error,
    }
}

fn minimal_job(body: &str) -> String {
    format!("version = 1\n[browsers.default]\nimage = \"img\"\n[jobs.one]\n{body}\n")
}

fn invalid_path(error: &ConfigError) -> &str {
    match error {
        ConfigError::InvalidValue { path, .. } => path,
        other => panic!("expected an invalid value, got {other:?}"),
    }
}

fn toml_error(source: &str) -> ConfigError {
    match error(source) {
        ConfigError::Toml { message } => {
            assert!(message.contains("TOML parse error"), "{message}");
            ConfigError::Toml { message }
        }
        other => panic!("expected a TOML error, got {other:?}"),
    }
}

#[test]
fn a_full_file_resolves_with_defaults_applied() {
    let config = config(FULL);
    assert_eq!(config.browsers.len(), 2);
    assert_eq!(config.jobs.len(), 2);

    let mirror = resolve(&config, "mirror-docs").unwrap();
    assert_eq!(mirror.name.as_str(), "mirror-docs");
    assert_eq!(mirror.browser.as_str(), "default");
    assert_eq!(mirror.profile.as_str(), "docs");
    assert_eq!(mirror.image, "ref-alpine-chromium");
    assert_eq!(mirror.viewport, [1280, 800]);
    assert_eq!(mirror.cpus, 2);
    assert_eq!(mirror.memory_mib, 4096);
    match &mirror.kind {
        ResolvedKind::Grab(grab) => {
            assert_eq!(grab.urls, ["https://example.com/docs/", "http://example.com/more"]);
            assert_eq!(grab.max_pages, Some(500));
            assert_eq!(grab.jobs, Some(4));
            assert!(!grab.robots);
            assert_eq!(grab.max_runtime, Some(Duration::from_secs(1800)));
        }
        other => panic!("expected a grab job, got {other:?}"),
    }

    let products = resolve(&config, "extract-products").unwrap();
    assert_eq!(products.profile.as_str(), "extract-products");
    assert_eq!(products.viewport, [1920, 1080]);
    assert_eq!(products.cpus, 8);
    assert_eq!(products.memory_mib, 8192);
    match &products.kind {
        ResolvedKind::Exec(exec) => {
            assert_eq!(exec.argv, ["python3", "jobs/extract.py"]);
            assert_eq!(exec.cwd, Some(PathBuf::from("jobs")));
            assert_eq!(exec.timeout, Some(Duration::from_secs(600)));
        }
        other => panic!("expected an exec job, got {other:?}"),
    }
}

#[test]
fn grab_defaults_are_robots_true_and_the_job_name_as_profile() {
    let source = minimal_job("kind = \"grab\"\nbrowser = \"default\"\nurls = [\"https://example.com/\"]");
    let config = config(&source);
    let job = resolve(&config, "one").unwrap();
    assert_eq!(job.profile.as_str(), "one");
    assert_eq!(job.viewport, [1280, 800]);
    match job.kind {
        ResolvedKind::Grab(grab) => {
            assert!(grab.robots);
            assert_eq!(grab.jobs, None);
            assert_eq!(grab.max_runtime, None);
        }
        other => panic!("expected a grab job, got {other:?}"),
    }
}

#[test]
fn unknown_top_level_field_is_rejected() {
    let source = format!("bogus = 1\n{FULL}");
    assert!(matches!(toml_error(&source), ConfigError::Toml { .. }));
}

#[test]
fn unknown_field_inside_a_grab_job_is_rejected() {
    let source = minimal_job("kind = \"grab\"\nbrowser = \"default\"\nurls = [\"https://example.com/\"]\nbogus = 1");
    toml_error(&source);
}

#[test]
fn argv_on_a_grab_job_is_rejected() {
    let source = minimal_job("kind = \"grab\"\nbrowser = \"default\"\nurls = [\"https://example.com/\"]\nargv = [\"true\"]");
    let ConfigError::Toml { message } = toml_error(&source) else { unreachable!() };
    assert!(message.contains("unknown field `argv`"), "{message}");
}

#[test]
fn urls_on_an_exec_job_are_rejected() {
    let source = minimal_job("kind = \"exec\"\nbrowser = \"default\"\nargv = [\"true\"]\nurls = [\"https://example.com/\"]");
    let ConfigError::Toml { message } = toml_error(&source) else { unreachable!() };
    assert!(message.contains("unknown field `urls`"), "{message}");
}

#[test]
fn an_unsupported_version_is_rejected() {
    let source = FULL.replace("version = 1", "version = 2");
    match error(&source) {
        ConfigError::UnsupportedVersion { found } => assert_eq!(found, 2),
        other => panic!("expected an unsupported version, got {other:?}"),
    }
}

#[test]
fn a_missing_version_is_rejected() {
    let source = FULL.replace("version = 1\n", "");
    toml_error(&source);
}

#[test]
fn a_job_naming_a_missing_browser_is_rejected() {
    let source = minimal_job("kind = \"grab\"\nbrowser = \"ghost\"\nurls = [\"https://example.com/\"]");
    match error(&source) {
        ConfigError::UnknownBrowser { job, browser } => {
            assert_eq!(job, "one");
            assert_eq!(browser, "ghost");
        }
        other => panic!("expected an unknown browser, got {other:?}"),
    }
}

#[test]
fn an_invalid_job_name_is_rejected() {
    let source = "version = 1\n[browsers.default]\nimage = \"img\"\n[jobs.Mirror]\nkind = \"grab\"\nbrowser = \"default\"\nurls = [\"https://example.com/\"]\n";
    match error(source) {
        ConfigError::InvalidName { table, source } => {
            assert_eq!(table, "jobs.Mirror");
            assert_eq!(source.name, "Mirror");
        }
        other => panic!("expected an invalid name, got {other:?}"),
    }
}

#[test]
fn an_invalid_browser_name_is_rejected() {
    let source = "version = 1\n[browsers.\"bad name\"]\nimage = \"img\"\n";
    match error(source) {
        ConfigError::InvalidName { table, source } => {
            assert_eq!(table, "browsers.bad name");
            assert_eq!(source.name, "bad name");
        }
        other => panic!("expected an invalid name, got {other:?}"),
    }
}

#[test]
fn an_invalid_profile_name_is_rejected() {
    let source = minimal_job("kind = \"exec\"\nbrowser = \"default\"\nprofile = \"Bad Profile\"\nargv = [\"true\"]");
    match error(&source) {
        ConfigError::InvalidName { table, source } => {
            assert_eq!(table, "jobs.one.profile");
            assert_eq!(source.name, "Bad Profile");
        }
        other => panic!("expected an invalid name, got {other:?}"),
    }
}

#[test]
fn empty_urls_are_rejected() {
    let source = minimal_job("kind = \"grab\"\nbrowser = \"default\"\nurls = []");
    let error = error(&source);
    assert_eq!(invalid_path(&error), "jobs.one.urls");
}

#[test]
fn a_non_http_url_is_rejected() {
    let source = minimal_job("kind = \"grab\"\nbrowser = \"default\"\nurls = [\"ftp://example.com/\"]");
    let error = error(&source);
    assert_eq!(invalid_path(&error), "jobs.one.urls[0]");
}

#[test]
fn empty_argv_is_rejected() {
    let source = minimal_job("kind = \"exec\"\nbrowser = \"default\"\nargv = []");
    let error = error(&source);
    assert_eq!(invalid_path(&error), "jobs.one.argv");
}

#[test]
fn an_empty_argv0_is_rejected() {
    let source = minimal_job("kind = \"exec\"\nbrowser = \"default\"\nargv = [\"\", \"x\"]");
    let error = error(&source);
    assert_eq!(invalid_path(&error), "jobs.one.argv[0]");
}

#[test]
fn an_exec_cwd_that_escapes_the_config_directory_is_rejected() {
    let source = minimal_job("kind = \"exec\"\nbrowser = \"default\"\nargv = [\"true\"]\ncwd = \"../jobs\"");
    let error = error(&source);
    assert_eq!(invalid_path(&error), "jobs.one.cwd");
}

#[test]
fn an_absolute_exec_cwd_is_rejected() {
    let source = minimal_job("kind = \"exec\"\nbrowser = \"default\"\nargv = [\"true\"]\ncwd = \"/jobs\"");
    let error = error(&source);
    assert_eq!(invalid_path(&error), "jobs.one.cwd");
}

#[test]
fn a_nested_parent_component_in_cwd_is_rejected() {
    let source = minimal_job("kind = \"exec\"\nbrowser = \"default\"\nargv = [\"true\"]\ncwd = \"a/../../b\"");
    let error = error(&source);
    assert_eq!(invalid_path(&error), "jobs.one.cwd");
}

#[test]
fn malformed_durations_are_rejected() {
    for text in ["", "30", "0s", "1.5s", "30m30s", "30mm", "-5m", "5 s", "m", "thirty"] {
        let source = minimal_job(&format!("kind = \"grab\"\nbrowser = \"default\"\nurls = [\"https://example.com/\"]\nmax_runtime = \"{text}\""));
        let error = error(&source);
        assert_eq!(invalid_path(&error), "jobs.one.max_runtime", "{text:?}");
    }
}

#[test]
fn durations_accept_each_unit() {
    for (text, seconds) in [("90s", 90), ("30m", 1800), ("2h", 7200)] {
        let source = minimal_job(&format!("kind = \"exec\"\nbrowser = \"default\"\nargv = [\"true\"]\ntimeout = \"{text}\""));
        let config = config(&source);
        match resolve(&config, "one").unwrap().kind {
            ResolvedKind::Exec(exec) => assert_eq!(exec.timeout, Some(Duration::from_secs(seconds))),
            other => panic!("expected an exec job, got {other:?}"),
        }
    }
}

#[test]
fn out_of_range_viewports_are_rejected() {
    for viewport in ["[319, 800]", "[1280, 7681]", "[0, 0]"] {
        let source = format!("version = 1\n[browsers.default]\nimage = \"img\"\nviewport = {viewport}\n");
        let error = error(&source);
        assert_eq!(invalid_path(&error), "browsers.default.viewport", "{viewport}");
    }
}

#[test]
fn out_of_range_cpus_are_rejected() {
    for cpus in ["0", "17"] {
        let source = format!("version = 1\n[browsers.default]\nimage = \"img\"\ncpus = {cpus}\n");
        let error = error(&source);
        assert_eq!(invalid_path(&error), "browsers.default.cpus", "{cpus}");
    }
}

#[test]
fn out_of_range_memory_is_rejected() {
    for memory in ["1023", "65537"] {
        let source = format!("version = 1\n[browsers.default]\nimage = \"img\"\nmemory_mib = {memory}\n");
        let error = error(&source);
        assert_eq!(invalid_path(&error), "browsers.default.memory_mib", "{memory}");
    }
}

#[test]
fn an_empty_image_is_rejected() {
    let source = "version = 1\n[browsers.default]\nimage = \"\"\n";
    let error = error(source);
    assert_eq!(invalid_path(&error), "browsers.default.image");
}

#[test]
fn out_of_range_page_workers_are_rejected() {
    for jobs in ["0", "65"] {
        let source = minimal_job(&format!("kind = \"grab\"\nbrowser = \"default\"\nurls = [\"https://example.com/\"]\njobs = {jobs}"));
        let error = error(&source);
        assert_eq!(invalid_path(&error), "jobs.one.jobs", "{jobs}");
    }
}

#[test]
fn resolving_an_unknown_job_is_rejected() {
    let config = config(FULL);
    match resolve(&config, "nope") {
        Err(ConfigError::NoSuchJob { name }) => assert_eq!(name, "nope"),
        other => panic!("expected no such job, got {other:?}"),
    }
    match resolve(&config, "Not A Job") {
        Err(ConfigError::NoSuchJob { name }) => assert_eq!(name, "Not A Job"),
        other => panic!("expected no such job, got {other:?}"),
    }
}

#[test]
fn load_reads_a_file_from_disk() {
    let dir = std::env::temp_dir().join(format!("bctl-jobs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bctl.toml");
    std::fs::write(&path, FULL).unwrap();
    let config = load(&path).unwrap();
    assert_eq!(config.jobs.len(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_reports_a_missing_file() {
    let path = std::path::Path::new("/nonexistent/bctl.toml");
    assert!(matches!(load(path), Err(ConfigError::Read { .. })));
}

#[test]
fn errors_display_where_the_problem_is() {
    let error = error("version = 1\n[browsers.default]\nimage = \"img\"\ncpus = 0\n");
    assert_eq!(error.to_string(), "invalid value at browsers.default.cpus: must be between 1 and 16");
    assert!(std::error::Error::source(&error).is_none());
}
