mod common;

use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use common::{Home, assert_ok, image, nanos, status_of};

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and the BCTL_TEST_IMAGE image (default helium) in .work/images"]
fn the_daemon_owns_sessions_across_restarts() {
    let home = Home::new("c3");
    let p = home.prefix();
    let (a, b, c) = (format!("{p}-a"), format!("{p}-b"), format!("{p}-c"));

    let starting = {
        let (home_path, c) = (home.path.clone(), c.clone());
        thread::spawn(move || {
            Command::new(env!("CARGO_BIN_EXE_bctl"))
                .args(["up", &c, "--profile", "pc", "--image", &image()])
                .env("BCTL_HOME", home_path)
                .output()
                .unwrap()
        })
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let seen_stage = loop {
        let status = home.json(&["status"]);
        if let Some(stage) = status.sessions.iter().find(|s| s.name == c).and_then(|s| s.stage.clone()) {
            break stage;
        }
        assert!(Instant::now() < deadline, "status never named a readiness stage for a starting session");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(["machine", "guest", "seat", "compositor", "browser", "sandbox", "cdp", "display"].contains(&seen_stage.as_str()));
    assert_ok(&starting.join().unwrap());

    let ups: Vec<_> = [(a.clone(), "pa"), (b.clone(), "pb")]
        .into_iter()
        .map(|(name, profile)| {
            let home_path = home.path.clone();
            thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_bctl"))
                    .args(["up", &name, "--profile", profile, "--image", &image()])
                    .env("BCTL_HOME", home_path)
                    .output()
                    .unwrap()
            })
        })
        .collect();
    for up in ups {
        assert_ok(&up.join().unwrap());
    }

    let before = home.json(&["status"]);
    let input = if nanos() { "headless" } else { "interactive" };
    for name in [&a, &b, &c] {
        let s = status_of(&before, name);
        assert_eq!((s.state.as_str(), s.generation, s.input.as_deref()), ("ready", 1, Some(input)), "{name}");
    }

    assert_ok(&home.up(&a, "pa"));
    assert_eq!(status_of(&home.json(&["status"]), &a).generation, 1, "a repeated up must not restart the session");

    let busy = home.json(&["up", &format!("{p}-d"), "--profile", "pa", "--image", &image()]);
    assert_eq!(busy.error.map(|e| e.code).as_deref(), Some("PROFILE_BUSY"));
    let conflict = home.json(&["up", &a, "--profile", "pb", "--image", &image()]);
    assert_eq!(conflict.error.map(|e| e.code).as_deref(), Some("CONFLICT"));

    let daemon = home.daemon_pid();
    assert!(!daemon.is_empty(), "no daemon holds the lock");
    assert!(Command::new("kill").args(["-9", &daemon]).status().unwrap().success());
    thread::sleep(Duration::from_millis(300));

    let after = home.json(&["status"]);
    assert_ne!(home.daemon_pid(), daemon, "status should have started a new daemon");
    for name in [&a, &b, &c] {
        let s = status_of(&after, name);
        assert_eq!(s.generation, 1, "{name} was restarted instead of adopted");
        assert!(matches!(s.state.as_str(), "ready" | "starting"), "{name} is {}", s.state);
        assert_eq!(s.machine.as_deref(), Some(format!("bctl-{name}-1").as_str()));
    }
    let mut machines = home.bctl_machines();
    machines.sort();
    let mut expected = vec![format!("bctl-{a}-1"), format!("bctl-{b}-1"), format!("bctl-{c}-1")];
    expected.sort();
    assert_eq!(machines, expected, "the restart duplicated or lost VMs");

    for name in [&a, &b, &c] {
        assert_ok(&home.bctl(&["down", name]));
    }
    assert!(home.bctl_machines().is_empty(), "down left VMs behind");
    assert_eq!(status_of(&home.json(&["status"]), &a).state, "stopped");
}
