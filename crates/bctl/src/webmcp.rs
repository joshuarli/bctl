//! Experimental WebMCP support using the existing lease-controlled page-eval operation.

use std::process::ExitCode;

use lexopt::prelude::*;
use miniserde::json;

use crate::paths::Paths;
use crate::protocol::{ActionRequest, Op, Request};

pub(crate) enum Command {
    List { session: String },
    Call { session: String, name: String, input: json::Value, allow_write: bool, allow_consequential: bool },
}

const USAGE: &str = "usage: bctl webmcp list SESSION | call SESSION TOOL [JSON_OBJECT] [--allow-write] [--allow-consequential]
  Results are JSON. The selected page must expose document.modelContext.
  Calls default to tools with readOnlyHint=true; writes and consequential actions need explicit opt-in.";

pub(crate) fn parse(mut parser: lexopt::Parser) -> Result<Command, lexopt::Error> {
    let verb = match parser.next()? {
        Some(Value(v)) => v.string()?,
        Some(other) => return Err(other.unexpected()),
        None => return Err(USAGE.into()),
    };
    if !matches!(verb.as_str(), "list" | "call") {
        return Err(USAGE.into());
    }

    let mut values = Vec::new();
    let (mut allow_write, mut allow_consequential) = (false, false);
    while let Some(arg) = parser.next()? {
        match arg {
            Long("allow-write") if verb == "call" => allow_write = true,
            Long("allow-consequential") if verb == "call" => allow_consequential = true,
            Long("json") => {} // Results are always a JSON value.
            Value(v) => values.push(v.string()?),
            other => return Err(other.unexpected()),
        }
    }
    if verb == "list" {
        if values.len() != 1 {
            return Err(USAGE.into());
        }
        return Ok(Command::List { session: values.remove(0) });
    }
    if !(2..=3).contains(&values.len()) {
        return Err(USAGE.into());
    }
    let session = values.remove(0);
    let name = values.remove(0);
    if name.is_empty() {
        return Err("WebMCP tool name cannot be empty".into());
    }
    let raw = values.pop().unwrap_or_else(|| "{}".into());
    if raw.len() > 16 * 1024 {
        return Err("WebMCP input exceeds 16 KiB".into());
    }
    let input: json::Value = json::from_str(&raw).map_err(|_| "WebMCP input must be valid JSON")?;
    if !matches!(&input, json::Value::Object(_)) {
        return Err("WebMCP input must be a JSON object".into());
    }
    Ok(Command::Call { session, name, input, allow_write, allow_consequential })
}

const LIST: &str = r#"(async () => {
  const context = document.modelContext;
  if (!context || typeof context.getTools !== "function") {
    throw new Error("WEBMCP_UNAVAILABLE: document.modelContext.getTools is not enabled for this page");
  }
  const tools = await context.getTools();
  return tools.filter(tool => tool.origin === location.origin).map(tool => ({
    name: tool.name,
    title: tool.title,
    description: tool.description,
    origin: tool.origin,
    inputSchema: tool.inputSchema,
    outputSchema: tool.outputSchema,
    annotations: tool.annotations
  }));
})()"#;

fn expression(command: &Command) -> String {
    match command {
        Command::List { .. } => LIST.into(),
        Command::Call { name, input, allow_write, allow_consequential, .. } => format!(
            r#"(async () => {{
  const context = document.modelContext;
  if (!context || typeof context.getTools !== "function" || typeof context.executeTool !== "function") {{
    throw new Error("WEBMCP_UNAVAILABLE: document.modelContext is not enabled for this page");
  }}
  const matches = (await context.getTools()).filter(tool => tool.name === {name} && tool.origin === location.origin);
  if (matches.length !== 1) {{
    throw new Error("WEBMCP_TOOL_MISSING_OR_AMBIGUOUS: rediscover after navigation");
  }}
  const tool = matches[0];
  if (tool.annotations?.readOnlyHint !== true && !{allow_write}) {{
    throw new Error("WEBMCP_WRITE_OPT_IN_REQUIRED: pass --allow-write after authorizing the operation");
  }}
  if (tool.annotations?.consequentialHint === true && !{allow_consequential}) {{
    throw new Error("WEBMCP_CONSEQUENTIAL_OPT_IN_REQUIRED: pass --allow-consequential after authorizing the operation");
  }}
  const input = JSON.parse({input});
  const raw = await context.executeTool(tool, input);
  let result = raw;
  if (typeof raw === "string") {{
    try {{ result = JSON.parse(raw); }} catch (_) {{}}
  }}
  return {{ name: tool.name, origin: tool.origin, result }};
}})()"#,
            name = json::to_string(&json::Value::String(name.clone())),
            input = json::to_string(&json::Value::String(json::to_string(input))),
            allow_write = allow_write,
            allow_consequential = allow_consequential,
        ),
    }
}

pub(crate) fn run(paths: &Paths, command: &Command) -> ExitCode {
    let session = match command {
        Command::List { session } | Command::Call { session, .. } => session,
    };
    let request = Request {
        op: Op::PageEval.as_str().into(),
        session: Some(session.clone()),
        action: Some(Box::new(ActionRequest { expression: Some(expression(command)), ..ActionRequest::default() })),
        ..Request::empty()
    };
    match crate::call(paths, &request) {
        Ok(response) => {
            if let Some(error) = response.error.as_deref() {
                return crate::report(error, false);
            }
            let value = response.page.as_ref().and_then(|page| page.action.as_ref()).and_then(|action| action.value.as_ref());
            match value {
                Some(value) => {
                    println!("{}", json::to_string(value));
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("bctl webmcp: page evaluation returned no value");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => crate::report(&error, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_serializes_metadata_without_window_handles() {
        let source = expression(&Command::List { session: "demo".into() });
        assert!(source.contains("context.getTools()"));
        assert!(source.contains("tool.origin === location.origin"));
        assert!(!source.contains("window: tool.window"));
    }

    #[test]
    fn call_escapes_untrusted_names_and_json_arguments() {
        let name = "bad\"; globalThis.__injected = true; //".to_string();
        let input = json::from_str(r#"{"query":"\"; alert(1); //"}"#).unwrap();
        let command = Command::Call {
            session: "demo".into(),
            name: name.clone(),
            input,
            allow_write: false,
            allow_consequential: false,
        };
        let source = expression(&command);
        assert!(source.contains(&format!("tool.name === {}", json::to_string(&json::Value::String(name)))));
        assert!(source.contains(r#"JSON.parse("{\"query\":\"\\\"; alert(1); //\"}")"#));
        assert!(source.contains("readOnlyHint !== true && !false"));
        assert!(source.contains("consequentialHint === true && !false"));
    }

    #[test]
    fn opt_ins_remain_distinct() {
        let input = json::from_str("{}").unwrap();
        let command = Command::Call {
            session: "demo".into(),
            name: "submit".into(),
            input,
            allow_write: true,
            allow_consequential: false,
        };
        let source = expression(&command);
        assert!(source.contains("readOnlyHint !== true && !true"));
        assert!(source.contains("consequentialHint === true && !false"));
    }
}
