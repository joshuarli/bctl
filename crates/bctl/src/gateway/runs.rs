use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bctl_core::SessionName;
use h12tiny::util::BoxBody;
use http::{HeaderValue, Response, StatusCode, header};
use miniserde::Serialize;

use super::{json_response, respond, text};
use crate::authority::{Contested, Viewer};
use crate::daemon::Daemon;
use crate::protocol::{ErrorCode, Request, Response as Reply};
use crate::runs::RunFiles;

const LOG_CHUNK_BYTES: u64 = 64 * 1024;
const ARTIFACT_LIST_LIMIT: usize = 2000;
const DOWNLOAD_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

fn failure(reply: Reply) -> Response<BoxBody> {
    let Some(error) = reply.error else {
        return text(StatusCode::INTERNAL_SERVER_ERROR, "The daemon failed without saying why.");
    };
    let status = match error.code.as_str() {
        code if code == ErrorCode::NoSuchRun.as_str() || code == ErrorCode::NoSuchSession.as_str() => StatusCode::NOT_FOUND,
        code if code == ErrorCode::BadRequest.as_str() => StatusCode::BAD_REQUEST,
        code if code == ErrorCode::Internal.as_str() => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    };
    let message = match &error.remedy {
        Some(remedy) => format!("{} ({remedy})", error.message),
        None => error.message.clone(),
    };
    text(status, &message)
}

struct Refusal(Box<Response<BoxBody>>);

impl From<Response<BoxBody>> for Refusal {
    fn from(response: Response<BoxBody>) -> Self {
        Refusal(Box::new(response))
    }
}

fn ask(daemon: &Arc<Daemon>, request: Request) -> Result<Reply, Refusal> {
    daemon.handle(&request).map_err(|e| failure(*e.0).into())
}

fn parse_run(id: &str) -> Result<i64, Refusal> {
    id.parse::<i64>().map_err(|_| text(StatusCode::BAD_REQUEST, "Invalid run id.").into())
}

fn one_run(daemon: &Arc<Daemon>, id: i64) -> Result<crate::protocol::RunView, Refusal> {
    let reply = ask(daemon, Request { op: "runs".into(), run: Some(id), ..Request::empty() })?;
    reply.runs.into_iter().next().ok_or_else(|| text(StatusCode::NOT_FOUND, "No such run.").into())
}

pub(super) fn session_runs(daemon: &Arc<Daemon>, session: &str) -> Response<BoxBody> {
    let session = match SessionName::new(session) {
        Ok(session) => session,
        Err(e) => return text(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    match ask(daemon, Request { op: "runs".into(), session: Some(session.to_string()), ..Request::empty() }) {
        Ok(reply) => json_response(&reply.runs),
        Err(refusal) => *refusal.0,
    }
}

pub(super) fn run(daemon: &Arc<Daemon>, id: &str) -> Response<BoxBody> {
    match parse_run(id).and_then(|id| one_run(daemon, id)) {
        Ok(run) => json_response(&run),
        Err(refusal) => *refusal.0,
    }
}

/// Hands the run's session back to automation, if the caller holds or is taking control, then resumes the run.
pub(super) fn resume(daemon: &Arc<Daemon>, id: &str, viewer: &str) -> Response<BoxBody> {
    let result = parse_run(id).and_then(|id| {
        let run = one_run(daemon, id)?;
        let session = SessionName::new(&run.session).map_err(|e| text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
        if let Err(Contested) = daemon.authority.of(&session, |control| control.resume(&Viewer::new(viewer))) {
            return Err(text(StatusCode::CONFLICT, "Another viewer has control of this browser, or is taking it.").into());
        }
        let reply = ask(daemon, Request { op: "resume".into(), run: Some(id), ..Request::empty() })?;
        reply.runs.into_iter().next().ok_or_else(|| text(StatusCode::INTERNAL_SERVER_ERROR, "The daemon returned no run.").into())
    });
    match result {
        Ok(run) => json_response(&run),
        Err(refusal) => *refusal.0,
    }
}

#[derive(Serialize)]
struct LogChunk {
    start: u64,
    end: u64,
    text: String,
}

fn query_value<'r>(query: &'r str, key: &str) -> Option<&'r str> {
    query.split('&').find_map(|pair| pair.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
}

/// A bounded slice of the run's log. Without `offset` it is the tail; with one it is what follows, so a
/// client that passes back the previous `end` reads each byte once.
pub(super) fn log(daemon: &Arc<Daemon>, id: &str, query: &str) -> Response<BoxBody> {
    let result = parse_run(id).and_then(|id| {
        one_run(daemon, id)?;
        let offset = match query_value(query, "offset") {
            Some(value) => Some(value.parse::<u64>().map_err(|_| text(StatusCode::BAD_REQUEST, "Invalid offset."))?),
            None => None,
        };
        read_log_chunk(&RunFiles::new(daemon.run_dir(id)).log(), offset).map_err(|e| text(StatusCode::INTERNAL_SERVER_ERROR, &format!("Cannot read the log: {e}")).into())
    });
    match result {
        Ok(chunk) => json_response(&chunk),
        Err(refusal) => *refusal.0,
    }
}

fn read_log_chunk(path: &Path, offset: Option<u64>) -> std::io::Result<LogChunk> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LogChunk { start: 0, end: 0, text: String::new() }),
        Err(e) => return Err(e),
    };
    let length = file.metadata()?.len();
    let mut start = offset.unwrap_or_else(|| length.saturating_sub(LOG_CHUNK_BYTES)).min(length);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0u8; usize::try_from((length - start).min(LOG_CHUNK_BYTES)).expect("a chunk fits in memory")];
    file.read_exact(&mut bytes)?;
    if offset.is_none() {
        let skipped = bytes.iter().take_while(|b| **b & 0xC0 == 0x80).count();
        bytes.drain(..skipped);
        start += skipped as u64;
    }
    let kept = match std::str::from_utf8(&bytes) {
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        _ => bytes.len(),
    };
    bytes.truncate(kept);
    Ok(LogChunk { start, end: start + kept as u64, text: String::from_utf8_lossy(&bytes).into_owned() })
}

#[derive(Serialize)]
struct Artifact {
    path: String,
    size: u64,
}

#[derive(Serialize)]
struct ArtifactList {
    files: Vec<Artifact>,
    truncated: bool,
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<Artifact>) -> std::io::Result<bool> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if collect(root, &entry.path(), files)? {
                return Ok(true);
            }
        } else if kind.is_file() {
            if files.len() == ARTIFACT_LIST_LIMIT {
                return Ok(true);
            }
            let relative = entry.path().strip_prefix(root).expect("entries live under the root").to_string_lossy().into_owned();
            files.push(Artifact { path: relative, size: entry.metadata()?.len() });
        }
    }
    Ok(false)
}

pub(super) fn artifacts(daemon: &Arc<Daemon>, id: &str) -> Response<BoxBody> {
    let result = parse_run(id).and_then(|id| {
        one_run(daemon, id)?;
        let root = RunFiles::new(daemon.run_dir(id)).output();
        let mut files = Vec::new();
        match collect(&root, &root, &mut files) {
            Ok(truncated) => Ok(ArtifactList { files, truncated }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ArtifactList { files, truncated: false }),
            Err(e) => Err(text(StatusCode::INTERNAL_SERVER_ERROR, &format!("Cannot list the output: {e}")).into()),
        }
    });
    match result {
        Ok(list) => json_response(&list),
        Err(refusal) => *refusal.0,
    }
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn artifact_path(root: &Path, segments: &[&str]) -> Option<PathBuf> {
    if segments.is_empty() {
        return None;
    }
    let mut path = root.to_path_buf();
    for segment in segments {
        let segment = percent_decode(segment)?;
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(['/', '\\', '\0']) {
            return None;
        }
        path.push(segment);
        if fs::symlink_metadata(&path).ok()?.file_type().is_symlink() {
            return None;
        }
    }
    Some(path)
}

/// Captured pages are untrusted web content: they are only ever handed over as an opaque download.
pub(super) fn download(daemon: &Arc<Daemon>, id: &str, segments: &[&str]) -> Response<BoxBody> {
    let result = parse_run(id).and_then(|id| {
        one_run(daemon, id)?;
        let path = artifact_path(&RunFiles::new(daemon.run_dir(id)).output(), segments).ok_or_else(|| text(StatusCode::NOT_FOUND, "No such artifact."))?;
        let metadata = fs::metadata(&path).ok().filter(|m| m.is_file()).ok_or_else(|| text(StatusCode::NOT_FOUND, "No such artifact."))?;
        if metadata.len() > DOWNLOAD_LIMIT_BYTES {
            return Err(text(StatusCode::PAYLOAD_TOO_LARGE, "This artifact is too large to download through the dashboard; read it from the run's output directory.").into());
        }
        fs::read(&path).map_err(|e| text(StatusCode::INTERNAL_SERVER_ERROR, &format!("Cannot read the artifact: {e}")).into())
    });
    match result {
        Ok(body) => {
            let name: String = segments.last().and_then(|s| percent_decode(s)).unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
            let mut response = respond(StatusCode::OK, "application/octet-stream", body);
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")).expect("a sanitized name is a valid header"));
            headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'"));
            response
        }
        Err(refusal) => *refusal.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_paths_stay_inside_the_output_and_never_follow_links() {
        let root = std::env::temp_dir().join(format!("bctl-artifact-{}", std::process::id()));
        fs::create_dir_all(root.join("site")).unwrap();
        fs::write(root.join("site/index.html"), "x").unwrap();
        std::os::unix::fs::symlink("/etc", root.join("link")).unwrap();
        assert_eq!(artifact_path(&root, &["site", "index.html"]), Some(root.join("site/index.html")));
        assert_eq!(artifact_path(&root, &["site", "%2e%2e", "x"]), None);
        assert_eq!(artifact_path(&root, &["site%2Findex.html"]), None);
        assert_eq!(artifact_path(&root, &["link", "passwd"]), None);
        assert_eq!(artifact_path(&root, &[]), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_log_is_read_in_chunks_that_never_split_a_character() {
        let dir = std::env::temp_dir().join(format!("bctl-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log");
        let line = "é".repeat(40_000);
        fs::write(&path, &line).unwrap();
        let tail = read_log_chunk(&path, None).unwrap();
        assert!(tail.text.chars().all(|c| c == 'é') && tail.end == 80_000);
        let first = read_log_chunk(&path, Some(0)).unwrap();
        let rest = read_log_chunk(&path, Some(first.end)).unwrap();
        assert_eq!(first.text.len() + rest.text.len(), 80_000);
        assert!(read_log_chunk(&path, Some(rest.end)).unwrap().text.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
