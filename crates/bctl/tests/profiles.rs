mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{Home, assert_ok, status_of};
use miniserde::json;
use shadowdriver::crawler::{ChromiumSessionOptions, CrawlerSession, TabOptions};

const CYCLES: usize = 20;

async fn cookie_round_trip(cdp_url: &str, expect: Option<&str>, set: &str) -> Result<(), String> {
    let mut options = ChromiumSessionOptions::new(PathBuf::new(), PathBuf::new());
    options.control_url = Some(cdp_url.to_string());
    options.headless = false;
    options.stealth = false;
    options.identity_mode = shadowdriver::BrowserIdentityMode::BrowserNative;
    let session = CrawlerSession::ensure_chromium(options).await.map_err(|e| e.to_string())?;
    let tab = session
        .new_tab(TabOptions { preload_script: None, apply_stealth: false, viewport_width: 800, viewport_height: 600 })
        .await
        .map_err(|e| e.to_string())?;
    let cookies = json::to_string(&tab.get_cookies().await.map_err(|e| e.to_string())?);
    if let Some(expect) = expect
        && !cookies.contains(&format!(r#""name":"bctl","value":"{expect}""#))
        && !cookies.contains(&format!(r#""value":"{expect}""#))
    {
        return Err(format!("cookie bctl={expect} did not survive: {cookies}"));
    }
    let expires = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 86_400;
    let params = json::from_str::<json::Value>(&format!(
        r#"{{"name":"bctl","value":"{set}","domain":"example.com","path":"/","secure":true,"expires":{expires}}}"#
    ))
    .unwrap();
    tab.command("Network.setCookie", params).await.map_err(|e| e.to_string())?;
    tab.close().await.ok();
    session.close(false).await;
    Ok(())
}

fn browser_log_problems(home: &Home, machine: &str) -> String {
    let output = Command::new(home.path.join("runtime/smolvm/smolvm"))
        .args(["machine", "exec", "--name", machine, "--", "sh", "-c"])
        .arg("grep -iE 'database is locked|sqlite.*(error|corrupt)|corrupt' /run/bctl/browser.log || true")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
#[ignore = "boots real VMs: needs runtime/build.sh and images/build.sh ref-alpine-chromium"]
fn a_profile_keeps_its_cookies_across_twenty_down_up_cycles() {
    let home = Home::new("c4");
    let session = format!("{}-s", home.prefix());
    let mut previous: Option<String> = None;
    for cycle in 0..CYCLES {
        assert_ok(&home.up(&session, "keeper"));
        let status = home.json(&["status"]);
        let s = status_of(&status, &session);
        let cdp_url = home.cdp_url(&session);
        let machine = s.machine.clone().expect("a ready session has a machine");
        let value = format!("cycle-{cycle}");
        async_io::block_on(cookie_round_trip(&cdp_url, previous.as_deref(), &value)).unwrap_or_else(|e| panic!("cycle {cycle}: {e}"));
        let problems = browser_log_problems(&home, &machine);
        assert!(problems.is_empty(), "cycle {cycle}: browser log reports storage trouble:\n{problems}");
        assert_ok(&home.bctl(&["down", &session]));
        previous = Some(value);
        std::thread::sleep(Duration::from_millis(100));
    }
}
