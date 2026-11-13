use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use miniserde::{Deserialize, json};
use rawzip::{CompressionMethod, RECOMMENDED_BUFFER_SIZE, ZipArchive};
use sha2::{Digest, Sha256};

static EMBEDDED_MANIFEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/images.json"));
const BUILD_TARGET: &str = env!("BCTL_BUILD_TARGET");
const SPARSE_BLOCK: usize = 64 * 1024;
const MAX_REDIRECTS: usize = 5;

/// The release's image digests, embedded at build time from `BCTL_IMAGES_MANIFEST`.
#[derive(Deserialize)]
pub(crate) struct Manifest {
    schema: u32,
    target: String,
    release: String,
    base_url: String,
    images: BTreeMap<String, Image>,
}

#[derive(Deserialize)]
pub(crate) struct Image {
    archive: Archive,
    files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Archive {
    name: String,
    sha256: String,
}

impl Manifest {
    pub(crate) fn embedded() -> Result<Manifest, String> {
        if EMBEDDED_MANIFEST.is_empty() {
            return Err("this build has no image manifest; build bctl with BCTL_IMAGES_MANIFEST or install a release".into());
        }
        let text = std::str::from_utf8(EMBEDDED_MANIFEST).map_err(|_| "the embedded image manifest is not UTF-8")?;
        Manifest::parse(text, BUILD_TARGET)
    }

    fn parse(text: &str, target: &str) -> Result<Manifest, String> {
        let manifest: Manifest = json::from_str(text).map_err(|_| "the embedded image manifest is not valid JSON")?;
        if manifest.schema != 1 {
            return Err(format!("image manifest schema {} is not supported", manifest.schema));
        }
        if manifest.target != target {
            return Err(format!("the image manifest is for {}, but this bctl was built for {target}", manifest.target));
        }
        for image in manifest.images.values() {
            safe_name(&image.archive.name)?;
            for name in image.files.keys() {
                safe_name(name)?;
            }
        }
        Ok(manifest)
    }

    pub(crate) fn image(&self, name: &str) -> Result<&Image, String> {
        self.images.get(name).ok_or_else(|| format!("{} has no image {name:?}", self.release))
    }
}

fn safe_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty() && !name.starts_with('.') && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok { Ok(()) } else { Err(format!("the image manifest names an unsafe file {name:?}")) }
}

/// Installs `name` from the embedded manifest into `dir`, downloading its archive unless every file
/// is already present with its listed digest.
pub(crate) fn pull(dir: &Path, manifest: &Manifest, name: &str) -> Result<(), String> {
    let image = manifest.image(name)?;
    let url = format!("{}{}", manifest.base_url, image.archive.name);
    pull_with(dir, name, image, |partial| {
        eprintln!("downloading {url}");
        download(&url, partial)
    })
}

fn pull_with(dir: &Path, name: &str, image: &Image, fetch: impl FnOnce(&Path) -> Result<String, String>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let lock = File::create(dir.join(".pull.lock")).map_err(|e| format!("cannot create the pull lock in {}: {e}", dir.display()))?;
    lock.try_lock().map_err(|_| "another bctl image pull is running".to_string())?;
    if installed(dir, image) {
        eprintln!("{name} is already installed");
        return Ok(());
    }
    let partial = dir.join(format!(".{}.partial", image.archive.name));
    let mut cleanup = Cleanup(vec![partial.clone()]);
    let digest = fetch(&partial)?;
    if digest != image.archive.sha256 {
        return Err(format!("{} has SHA-256 {digest}, expected {}", image.archive.name, image.archive.sha256));
    }
    eprintln!("verifying {}", image.archive.name);
    install_archive(&partial, dir, image, &mut cleanup)?;
    eprintln!("installed {name} in {}", dir.display());
    Ok(())
}

/// Removes every listed path when dropped, so a failed pull leaves no partial or temporary files.
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn installed(dir: &Path, image: &Image) -> bool {
    image.files.iter().all(|(file, digest)| File::open(dir.join(file)).and_then(hash_reader).is_ok_and(|actual| &actual == digest))
}

fn hash_reader(mut reader: impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn allowed(uri: &http::Uri) -> Result<(), String> {
    let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
    match uri.scheme_str() {
        Some("https") => Ok(()),
        Some("http") if host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) => Ok(()),
        _ => Err(format!("refusing to download {uri}: only https, or http to a loopback address, is allowed")),
    }
}

fn download(url: &str, partial: &Path) -> Result<String, String> {
    use h12tiny::client_sync::Client;
    let client = Client::builder().connect_timeout(Duration::from_secs(30)).map_err(|e| e.to_string())?.build();
    let mut uri: http::Uri = url.parse().map_err(|_| format!("{url:?} is not a URL"))?;
    for _ in 0..=MAX_REDIRECTS {
        allowed(&uri)?;
        let request = http::Request::get(uri.clone()).body(Vec::new()).map_err(|e| e.to_string())?;
        let mut response = client.request_with_timeout(request, Some(Duration::from_secs(60))).map_err(|e| format!("GET {uri}: {e}"))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response.headers().get(http::header::LOCATION).and_then(|v| v.to_str().ok()).ok_or_else(|| format!("GET {uri}: {status} without a Location"))?;
            uri = resolve(&uri, location)?;
            continue;
        }
        if status != http::StatusCode::OK {
            return Err(format!("GET {uri}: {status}"));
        }
        let body = response.body_mut();
        body.set_read_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
        let mut file = File::create(partial).map_err(|e| format!("cannot create {}: {e}", partial.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1 << 20];
        loop {
            let n = body.read(&mut buffer).map_err(|e| format!("GET {uri}: {e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            file.write_all(&buffer[..n]).map_err(|e| format!("cannot write {}: {e}", partial.display()))?;
        }
        if !body.is_complete() {
            return Err(format!("GET {uri}: the response ended early"));
        }
        return Ok(hex(&hasher.finalize()));
    }
    Err(format!("{url} redirected more than {MAX_REDIRECTS} times"))
}

fn resolve(base: &http::Uri, location: &str) -> Result<http::Uri, String> {
    let absolute = if location.starts_with('/') {
        format!("{}://{}{location}", base.scheme_str().unwrap_or("https"), base.authority().map(|a| a.as_str()).unwrap_or_default())
    } else {
        location.to_string()
    };
    absolute.parse().map_err(|_| format!("redirect to {location:?} is not a URL"))
}

fn install_archive(archive_path: &Path, dir: &Path, image: &Image, cleanup: &mut Cleanup) -> Result<(), String> {
    let bad = |e: rawzip::Error| format!("{} is not a valid zip: {e}", archive_path.display());
    let file = File::open(archive_path).map_err(|e| format!("cannot open {}: {e}", archive_path.display()))?;
    let mut buffer = vec![0; RECOMMENDED_BUFFER_SIZE];
    let archive = ZipArchive::from_file(file, &mut buffer).map_err(bad)?;
    let mut found = Vec::new();
    let mut names = BTreeSet::new();
    let mut entries = archive.entries(&mut buffer);
    while let Some(entry) = entries.next_entry().map_err(bad)? {
        let raw = entry.file_path();
        let name = std::str::from_utf8(raw.as_bytes()).ok().filter(|name| image.files.contains_key(*name));
        let Some(name) = name.filter(|_| !entry.is_dir()) else {
            return Err(format!("the image archive has an unexpected entry {:?}", String::from_utf8_lossy(raw.as_bytes())));
        };
        if !names.insert(name.to_string()) {
            return Err(format!("the image archive lists {name} twice"));
        }
        let method = entry.compression_method();
        if method != CompressionMethod::STORE && method != CompressionMethod::DEFLATE {
            return Err(format!("{name} uses an unsupported compression method"));
        }
        found.push((name.to_string(), entry.wayfinder(), method, entry.uncompressed_size_hint()));
    }
    if let Some(missing) = image.files.keys().find(|name| !names.contains(*name)) {
        return Err(format!("the image archive has no {missing}"));
    }
    let mut staged = Vec::new();
    for (name, wayfinder, method, size) in found {
        let entry = archive.get_entry(wayfinder).map_err(bad)?;
        let temp = dir.join(format!(".{name}.pull"));
        cleanup.0.push(temp.clone());
        let digest = if method == CompressionMethod::DEFLATE {
            write_sparse(entry.verifying_reader(Inflater::new(entry.reader())), &temp, size)
        } else {
            write_sparse(entry.verifying_reader(entry.reader()), &temp, size)
        }
        .map_err(|e| format!("cannot extract {name}: {e}"))?;
        if digest != image.files[&name] {
            return Err(format!("{name} has SHA-256 {digest}, expected {}", image.files[&name]));
        }
        staged.push((temp, dir.join(&name)));
    }
    for (temp, target) in staged {
        std::fs::rename(&temp, &target).map_err(|e| format!("cannot install {}: {e}", target.display()))?;
    }
    File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("cannot sync {}: {e}", dir.display()))
}

/// Writes `reader` to a new file at `path`, seeking over aligned all-zero blocks so the file stays
/// sparse, and returns the SHA-256 of the full contents. The file is first extended to `size`
/// because APFS allocates a seeked-over hole when the next write arrives slowly, as it does
/// behind a decompressor, but never allocates a hole that lies inside the file's length.
fn write_sparse(mut reader: impl Read, path: &Path, size: u64) -> io::Result<String> {
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    file.set_len(size)?;
    let mut hasher = Sha256::new();
    let mut block = vec![0; SPARSE_BLOCK];
    let mut len = 0u64;
    loop {
        let n = read_full(&mut reader, &mut block)?;
        if n == 0 {
            break;
        }
        hasher.update(&block[..n]);
        if n == SPARSE_BLOCK && block.iter().all(|b| *b == 0) {
            file.seek(SeekFrom::Current(n as i64))?;
        } else {
            file.write_all(&block[..n])?;
        }
        len += n as u64;
    }
    file.set_len(len)?;
    file.sync_all()?;
    Ok(hex(&hasher.finalize()))
}

fn read_full(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Streams a raw DEFLATE body.
struct Inflater<R> {
    inner: R,
    inflate: zlib_rs::Inflate,
    input: Box<[u8]>,
    start: usize,
    end: usize,
    done: bool,
}

impl<R> Inflater<R> {
    fn new(inner: R) -> Self {
        Inflater { inner, inflate: zlib_rs::Inflate::new(false, 15), input: vec![0; 1 << 16].into_boxed_slice(), start: 0, end: 0, done: false }
    }
}

impl<R: Read> Read for Inflater<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.done {
            return Ok(0);
        }
        loop {
            let mut eof = false;
            if self.start == self.end {
                self.end = self.inner.read(&mut self.input)?;
                self.start = 0;
                eof = self.end == 0;
            }
            let (before_in, before_out) = (self.inflate.total_in(), self.inflate.total_out());
            let status = self
                .inflate
                .decompress(&self.input[self.start..self.end], out, zlib_rs::InflateFlush::NoFlush)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.as_str()))?;
            self.start += (self.inflate.total_in() - before_in) as usize;
            let produced = (self.inflate.total_out() - before_out) as usize;
            if status == zlib_rs::Status::StreamEnd {
                self.done = true;
                return Ok(produced);
            }
            if produced > 0 {
                return Ok(produced);
            }
            if eof {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the DEFLATE stream is truncated"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.work/tmp").join(format!("image-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("images")).unwrap();
            Scratch(root)
        }

        fn images(&self) -> PathBuf {
            self.0.join("images")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0; zlib_rs::compress_bound(data.len())];
        let config = zlib_rs::DeflateConfig { window_bits: -15, ..zlib_rs::DeflateConfig::new(6) };
        let (written, code) = zlib_rs::compress_slice(&mut out, data, config);
        assert_eq!(code, zlib_rs::ReturnCode::Ok);
        let len = written.len();
        out.truncate(len);
        out
    }

    struct Precompressed<'a, W: Write> {
        out: &'a mut W,
        plain: Vec<u8>,
        method: CompressionMethod,
    }

    impl<W: Write> Write for Precompressed<'_, W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.plain.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn zip(entries: &[(&[u8], &[u8], CompressionMethod)]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut archive = rawzip::ZipArchiveWriter::new(&mut output);
        for (name, data, method) in entries {
            let (mut entry, config) = archive.new_file(rawzip::path::EntryPath::verbatim(*name)).compression_method(*method).start().unwrap();
            let mut writer = config.wrap(Precompressed { out: &mut entry, plain: Vec::new(), method: *method });
            writer.write_all(data).unwrap();
            let (encoder, descriptor) = writer.finish().unwrap();
            let body = if encoder.method == CompressionMethod::DEFLATE { deflate(&encoder.plain) } else { encoder.plain };
            encoder.out.write_all(&body).unwrap();
            entry.finish(descriptor).unwrap();
        }
        archive.finish().unwrap();
        output
    }

    fn sha(data: &[u8]) -> String {
        hex(&Sha256::digest(data))
    }

    fn image(archive: &[u8], files: &[(&str, &[u8])]) -> Image {
        Image {
            archive: Archive { name: "test.zip".into(), sha256: sha(archive) },
            files: files.iter().map(|(name, data)| (name.to_string(), sha(data))).collect(),
        }
    }

    fn serve(bytes: Vec<u8>) -> impl FnOnce(&Path) -> Result<String, String> {
        move |partial| {
            std::fs::write(partial, &bytes).unwrap();
            Ok(sha(&bytes))
        }
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n != ".pull.lock").collect();
        names.sort();
        names
    }

    const DISK: &[u8] = b"disk image bytes";
    const PROFILE: &[u8] = b"profile image bytes, profile image bytes, profile image bytes";

    #[test]
    fn installs_store_and_deflate_entries_then_skips_the_download() {
        let scratch = Scratch::new("ok");
        let bytes = zip(&[(b"t.img", DISK, CompressionMethod::STORE), (b"t.profile.img", PROFILE, CompressionMethod::DEFLATE)]);
        let image = image(&bytes, &[("t.img", DISK), ("t.profile.img", PROFILE)]);
        pull_with(&scratch.images(), "t", &image, serve(bytes)).unwrap();
        assert_eq!(std::fs::read(scratch.images().join("t.img")).unwrap(), DISK);
        assert_eq!(std::fs::read(scratch.images().join("t.profile.img")).unwrap(), PROFILE);
        assert_eq!(leftovers(&scratch.images()), ["t.img", "t.profile.img"]);
        pull_with(&scratch.images(), "t", &image, |_| panic!("an installed image was downloaded again")).unwrap();
    }

    #[test]
    fn a_changed_file_is_reinstalled() {
        let scratch = Scratch::new("changed");
        let bytes = zip(&[(b"t.img", DISK, CompressionMethod::DEFLATE)]);
        let image = image(&bytes, &[("t.img", DISK)]);
        std::fs::write(scratch.images().join("t.img"), b"stale").unwrap();
        pull_with(&scratch.images(), "t", &image, serve(bytes)).unwrap();
        assert_eq!(std::fs::read(scratch.images().join("t.img")).unwrap(), DISK);
    }

    fn rejects(tag: &str, entries: &[(&[u8], &[u8], CompressionMethod)], expected: &[(&str, &[u8])], tamper: impl FnOnce(&mut Image), message: &str) {
        let scratch = Scratch::new(tag);
        std::fs::write(scratch.images().join("t.img"), b"old").unwrap();
        let bytes = zip(entries);
        let mut image = image(&bytes, expected);
        tamper(&mut image);
        let error = pull_with(&scratch.images(), "t", &image, serve(bytes)).unwrap_err();
        assert!(error.contains(message), "{error}");
        assert_eq!(std::fs::read(scratch.images().join("t.img")).unwrap(), b"old");
        assert_eq!(leftovers(&scratch.images()), ["t.img"]);
    }

    #[test]
    fn an_archive_digest_mismatch_is_rejected() {
        rejects("archive", &[(b"t.img", DISK, CompressionMethod::STORE)], &[("t.img", DISK)], |i| i.archive.sha256 = sha(b"other"), "test.zip has SHA-256");
    }

    #[test]
    fn a_file_digest_mismatch_is_rejected() {
        rejects("file", &[(b"t.img", DISK, CompressionMethod::DEFLATE)], &[("t.img", b"other")], |_| {}, "t.img has SHA-256");
    }

    #[test]
    fn an_extra_entry_is_rejected() {
        rejects(
            "extra",
            &[(b"t.img", DISK, CompressionMethod::STORE), (b"t.extra", DISK, CompressionMethod::STORE)],
            &[("t.img", DISK)],
            |_| {},
            "unexpected entry \"t.extra\"",
        );
    }

    #[test]
    fn a_traversal_name_is_rejected() {
        rejects("traversal", &[(b"../t.img", DISK, CompressionMethod::STORE)], &[("t.img", DISK)], |_| {}, "unexpected entry \"../t.img\"");
    }

    #[test]
    fn a_missing_entry_is_rejected() {
        rejects(
            "missing",
            &[(b"t.img", DISK, CompressionMethod::STORE)],
            &[("t.img", DISK), ("t.profile.img", PROFILE)],
            |_| {},
            "has no t.profile.img",
        );
    }

    #[test]
    fn a_mostly_zero_image_installs_sparse() {
        let scratch = Scratch::new("sparse");
        let mut disk = vec![0u8; 64 << 20];
        disk[..4096].fill(0xa5);
        disk[(64 << 20) - 17] = 1;
        let bytes = zip(&[(b"t.img", &disk, CompressionMethod::DEFLATE)]);
        let image = image(&bytes, &[("t.img", &disk)]);
        pull_with(&scratch.images(), "t", &image, serve(bytes)).unwrap();
        let path = scratch.images().join("t.img");
        assert_eq!(std::fs::read(&path).unwrap(), disk);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), disk.len() as u64);
        assert!(meta.blocks() * 512 < 4 << 20, "{} bytes allocated", meta.blocks() * 512);
    }

    #[test]
    fn manifests_for_another_target_or_with_unsafe_names_are_rejected() {
        let manifest = |target: &str, file: &str| {
            format!(r#"{{"schema":1,"target":"{target}","release":"release-x","base_url":"https://example.invalid/","images":{{"i":{{"archive":{{"name":"i.zip","sha256":"00"}},"files":{{"{file}":"00"}}}}}}}}"#)
        };
        assert!(Manifest::parse(&manifest("aarch64-apple-darwin", "i.img"), "aarch64-apple-darwin").is_ok());
        let error = Manifest::parse(&manifest("x86_64-unknown-linux-musl", "i.img"), "aarch64-apple-darwin").err().unwrap();
        assert!(error.contains("built for aarch64-apple-darwin"), "{error}");
        assert!(Manifest::parse(&manifest("aarch64-apple-darwin", "../i.img"), "aarch64-apple-darwin").is_err());
    }

    #[test]
    fn only_https_or_loopback_http_is_downloaded() {
        let ok = |url: &str| allowed(&url.parse().unwrap()).is_ok();
        assert!(ok("https://github.com/a"));
        assert!(ok("http://127.0.0.1:8000/a"));
        assert!(ok("http://[::1]:8000/a"));
        assert!(ok("http://localhost/a"));
        assert!(!ok("http://github.com/a"));
        assert!(!ok("ftp://127.0.0.1/a"));
        assert_eq!(resolve(&"https://a.example/x/y".parse().unwrap(), "/z?q").unwrap().to_string(), "https://a.example/z?q");
    }
}
