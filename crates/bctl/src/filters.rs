use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use bctl_cdp::protocol::{AttachToTarget, Command, Effect, GetTargets};
use bctl_cdp::{Connection, Generation, Scope};
use futures_lite::future::block_on;
use miniserde::{Deserialize, Serialize};

// uBlock Origin holds every request a tab makes until its filter lists are compiled, then reloads the tabs it
// held, so a navigation sent before then stalls or ends in ERR_ABORTED. Its background page publishes
// `µBlock.readyToFilter` once it filters.
const UBLOCK_BACKGROUND: &str = "chrome-extension://blockjmkbacgjkknlgpkjjiijinjdanf/";
const READY_TO_FILTER: &str = "self.µBlock !== undefined && µBlock.readyToFilter === true";
const BACKGROUND_GRACE: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(100);
const CALL: Duration = Duration::from_secs(5);

#[derive(Serialize)]
struct ReadValue {
    expression: &'static str,
    #[serde(rename = "returnByValue")]
    return_by_value: bool,
}

#[derive(Deserialize)]
struct Value {
    value: Option<bool>,
}

#[derive(Deserialize)]
struct Evaluated {
    result: Value,
}

impl Command for ReadValue {
    const METHOD: &'static str = "Runtime.evaluate";
    const EFFECT: Effect = Effect::ReadOnly;
    type Returns = Evaluated;
}

/// Waits until the browser's content filter lets requests through, so that the first navigation after ready is
/// neither held nor reloaded. A browser with no such filter is ready at once.
pub(crate) fn wait(cdp_socket: &Path, generation: Generation, deadline: Instant) -> Result<(), String> {
    let connection = crate::tabs::open(cdp_socket, generation)?;
    let waited = block_on(wait_on(&connection, deadline));
    block_on(connection.shutdown());
    waited
}

async fn wait_on(connection: &Connection, deadline: Instant) -> Result<(), String> {
    let grace = Instant::now() + BACKGROUND_GRACE;
    let target = loop {
        let targets = connection.call(Scope::Browser, &GetTargets {}, Instant::now() + CALL).await.map_err(|e| e.to_string())?;
        if let Some(target) = targets.target_infos.into_iter().find(|t| t.url.starts_with(UBLOCK_BACKGROUND)) {
            break target;
        }
        if Instant::now() >= grace {
            return Ok(());
        }
        thread::sleep(POLL);
    };
    let attached = connection.call(Scope::Browser, &AttachToTarget::new(target.target_id), Instant::now() + CALL).await.map_err(|e| e.to_string())?;
    let read = ReadValue { expression: READY_TO_FILTER, return_by_value: true };
    loop {
        let evaluated = connection.call(Scope::Session(&attached.session_id), &read, Instant::now() + CALL).await.map_err(|e| e.to_string())?;
        if evaluated.result.value == Some(true) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("uBlock Origin never finished loading its filter lists".into());
        }
        thread::sleep(POLL);
    }
}
