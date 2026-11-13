# Files

A session's browser runs in a guest with its own filesystem, so a host path
means nothing to it. A CDP client that passes one to `DOM.setFileInputFiles`
uploads nothing, and a download lands in the guest, not under
`BCTL_OUTPUT_DIR`.

Each session has a files directory that both sides see:

| Side | Path | Reported as |
|------|------|-------------|
| Host | `<BCTL_HOME>/files/<session>` | `files_dir` in `bctl inspect NAME --json`, `BCTL_FILES_DIR` in a job |
| Guest | `/home/browser/Downloads` | `guest_files_dir`, `BCTL_GUEST_FILES_DIR` |

The guest path is the browser's default download directory, so:

- **Downloads** appear in `files_dir` as the browser writes them.
- **Uploads** are a copy into `files_dir`, then the guest path given to the
  client.

`bctl up` creates the directory, readable only by the host user, and
`bctl rm` deletes it with its contents. `bctl down` and `up` keep it.

SmolVM mounts it with its own host mount. Nanos mounts it over virtio-fs, with
the tag `files`. On Linux, the session's supervisor runs `virtiofsd` with its
namespace sandbox, which confines it to that one directory. On the Mac,
libkrun serves it.

## Downloads

Under the browser's default download behavior, Helium holds a download from
a plain-HTTP URL for a person to confirm: it stays `NAME.crdownload`,
complete but never renamed. A client that wants downloads to finish sets
`Browser.setDownloadBehavior` to `allow` with `downloadPath`
`/home/browser/Downloads`, a guest path. That setting is the browser's, so it
lasts until a client changes it.

## With the clients

**browser-use 1.0.0** needs no files directory for uploads: its
`upload_file` sends the bytes through the page when the browser holds a path
it cannot read. It leaves download behavior alone, so set it with
`cdp('Browser.setDownloadBehavior', behavior='allow', downloadPath='/home/browser/Downloads')`.

**agent-browser 0.43.0** passes paths through to the browser, so upload by
guest path:

```sh
cp report.pdf "$BCTL_FILES_DIR/"
agent-browser upload '#file' "$BCTL_GUEST_FILES_DIR/report.pdf"
```

Attached over `--cdp`, it ignores `AGENT_BROWSER_DOWNLOAD_PATH` and refuses
its own `download` command. A download it starts follows the browser's
behavior (see Downloads).

## Trust

The guest can write anything into the files directory, including symlinks
that point elsewhere on the host. Treat its contents as untrusted, and do
not follow its symlinks with host tools.

## Session capabilities

`bctl inspect NAME --json` reports what a session's runtime can do, each as
its own field, once the session holds a VM:

| Field | SmolVM | Nanos |
|-------|--------|-------|
| `display` | `headed` | `headless` |
| `input` | `interactive` or `view-only` | `headless`: no keyboard or pointer |
| `pausable` | `true` | `false`: `bctl pause` fails with `UNSUPPORTED_BY_RUNTIME` |
| `files_dir`, `guest_files_dir` | present | absent |

`files_dir` and `guest_files_dir` are reported in every state, since the
directory outlives the VM.
