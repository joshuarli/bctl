use std::fmt::Write;
use std::path::{Path, PathBuf};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() { files(&path, out) } else { out.push(path) }
    }
}

fn main() {
    let dist = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if !dist.join("index.html").exists() {
        panic!("the dashboard is not built: run `pnpm --dir web install && pnpm --dir web build` first ({})", dist.display());
    }
    let mut paths = Vec::new();
    files(&dist, &mut paths);
    paths.sort();
    let mut generated = String::from("static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for path in &paths {
        println!("cargo:rerun-if-changed={}", path.display());
        let url = format!("/{}", path.strip_prefix(&dist).unwrap().to_string_lossy());
        writeln!(generated, "    ({url:?}, {:?}, include_bytes!({:?})),", content_type(path), path.display().to_string()).unwrap();
    }
    generated.push_str("];\n");
    std::fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("assets.rs"), generated).unwrap();
}
