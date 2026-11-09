use miniserde::json::{self, Value};

use crate::host::Fallible;
use crate::net::{self, Stream, WebSocket};

pub struct Cdp {
    socket: WebSocket,
    next_id: u64,
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(fields) => fields.get(key),
        _ => None,
    }
}

fn string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let mut at = value;
    for key in keys {
        at = field(at, key)?;
    }
    match at {
        Value::String(s) => Some(s),
        _ => None,
    }
}

pub enum Endpoint {
    Tcp { port: u16, host: String },
    Unix { path: std::path::PathBuf },
}

impl Endpoint {
    fn open(&self) -> Fallible<(Stream, String)> {
        match self {
            Endpoint::Tcp { port, host } => Ok((net::tcp("127.0.0.1", *port)?, host.clone())),
            Endpoint::Unix { path } => Ok((net::unix(path)?, "localhost".to_string())),
        }
    }

    pub fn get_json(&self, path: &str) -> Fallible<Value> {
        let (mut stream, host) = self.open()?;
        let response = net::request(&mut stream, "GET", &host, path, &[], b"")?;
        if response.status != 200 {
            return Err(format!("GET {path} answered {}: {}", response.status, response.text()));
        }
        json::from_str(&response.text()).map_err(|_| format!("GET {path} returned bad JSON"))
    }

    pub fn browser_version(&self) -> Fallible<String> {
        let version = self.get_json("/json/version")?;
        string(&version, &["Browser"]).map(str::to_string).ok_or_else(|| "/json/version has no Browser".into())
    }

    fn websocket_path(&self, url: &str) -> Fallible<String> {
        let rest = url.strip_prefix("ws://").ok_or_else(|| format!("unexpected websocket url {url}"))?;
        Ok(rest.find('/').map_or("/".into(), |at| rest[at..].to_string()))
    }

    pub fn attach_browser(&self) -> Fallible<Cdp> {
        let version = self.get_json("/json/version")?;
        let url = string(&version, &["webSocketDebuggerUrl"]).ok_or("no webSocketDebuggerUrl")?;
        self.attach(&self.websocket_path(url)?)
    }

    pub fn attach_first_page(&self) -> Fallible<(String, Cdp)> {
        let list = self.get_json("/json/list")?;
        let Value::Array(targets) = &list else { return Err("/json/list is not an array".into()) };
        let page = targets.iter().find(|t| string(t, &["type"]) == Some("page")).ok_or("no page target")?;
        let id = string(page, &["id"]).ok_or("page has no id")?.to_string();
        let url = string(page, &["webSocketDebuggerUrl"]).ok_or("page has no webSocketDebuggerUrl")?;
        Ok((id, self.attach(&self.websocket_path(url)?)?))
    }

    pub fn attach(&self, path: &str) -> Fallible<Cdp> {
        let (stream, host) = self.open()?;
        Ok(Cdp { socket: WebSocket::upgrade(stream, &host, path, &[])?, next_id: 0 })
    }
}

impl Cdp {
    pub fn call(&mut self, method: &str, params: &str) -> Fallible<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.socket.send_text(&format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#)).map_err(|e| format!("{method}: {e}"))?;
        loop {
            let message = self.socket.receive().map_err(|e| format!("{method}: {e}"))?;
            let value: Value = json::from_str(&String::from_utf8_lossy(&message)).map_err(|_| format!("{method}: bad JSON reply"))?;
            let answers = matches!(field(&value, "id"), Some(Value::Number(json::Number::U64(n))) if *n == id);
            if !answers {
                continue;
            }
            if let Some(error) = field(&value, "error") {
                return Err(format!("{method} failed: {}", json::to_string(error)));
            }
            return Ok(field(&value, "result").cloned().unwrap_or(Value::Null));
        }
    }

    pub fn evaluate(&mut self, expression: &str) -> Fallible<String> {
        let params = format!(r#"{{"expression":{},"returnByValue":true,"awaitPromise":true}}"#, json::to_string(&Value::String(expression.into())));
        let result = self.call("Runtime.evaluate", &params)?;
        if let Some(exception) = field(&result, "exceptionDetails") {
            return Err(format!("evaluating {expression:?} threw {}", json::to_string(exception)));
        }
        match field(&result, "result").and_then(|r| field(r, "value")) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(other) => Ok(json::to_string(other)),
            None => Ok(String::new()),
        }
    }

    pub fn navigate(&mut self, url: &str) -> Fallible<()> {
        self.call("Page.navigate", &format!(r#"{{"url":{}}}"#, json::to_string(&Value::String(url.into())))).map(drop)
    }
}
