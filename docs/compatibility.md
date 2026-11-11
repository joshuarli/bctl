# External client compatibility

What each pinned client is verified to do against bctl, per host and
runtime. A row is supported only on the tuples where `tests/interop/run.sh`
passed it. Anything not listed is unverified.

## Verified tuples

| Tuple | Host | Runtime | Image | Checks |
|-------|------|---------|-------|--------|
| mac-smolvm | macOS 27.0.1, Apple M5 Max (arm64) | smolvm 1.34.0 | `helium` sha256:a91a0ab8… | 113 of 113 |
| mac-nanos | macOS 27.0.1, Apple M5 Max (arm64) | Nanos on libkrun, Hydrogen `bbeea356` | `nanos-helium` sha256:f6db6190… | 105 of 105, twice |

Every tuple uses Helium 0.20.3.1 and the same client pins:

- browser-use 1.0.0 (`070be98`), with browser-harness 0.1.13
- agent-browser 0.42.0 (`e751995`)

Linux x86_64 with Nanos is unverified on this bctl revision.

Reproduce a tuple:

```sh
BCTL_RUNTIME=smolvm sh tests/interop/run.sh
BCTL_RUNTIME=nanos sh tests/interop/run.sh
```

## Features

✓ passed, – not applicable, ? not run.

| Feature | browser-use | agent-browser | mac-smolvm | mac-nanos |
|---------|:-----------:|:-------------:|:----------:|:---------:|
| Attach over a `bctl cdp-url` capability URL | ✓ | ✓ | ✓ | ✓ |
| Navigate, fill, click; the fixture sees the submission | ✓ | ✓ | ✓ | ✓ |
| Accessibility tree, screenshot | ✓ | ✓ | ✓ | ✓ |
| Dialogs, popups, same-site and cross-site frames | ✓ | ✓ | ✓ | ✓ |
| 10 attach and detach cycles, no socket left open | ✓ | ✓ | ✓ | ✓ |
| browser-use's domain guard and its second CDP socket | ✓ | – | ✓ | ✓ |
| Revocation closes the client's sockets | ✓ | ✓ | ✓ | ✓ |
| Run as a `bctl run` job: result, record, cancel, no retry | ✓ | ✓ | ✓ | ✓ |
| Upload a host file | ✓ through the page | – | ✓ | ? |
| Upload from the files directory by guest path | – | ✓ | ✓ | – no files directory |
| Download into the files directory | ✓ | ? | ✓ | – no files directory |
| `--provider bctl`: owned launch, close, reclaim after owner death | – | ✓ | ✓ | ✓ |
| `--provider bctl`: borrowed lease, close revokes only its URL | – | ✓ | ✓ | ✓ |
| `--provider bctl`: refuses other engines, `headed` on Nanos | – | ✓ | ✓ | ✓ |

agent-browser downloads have no check. The other `?` cell is a check the
suite skips on Nanos, where it stops the file checks early. So is the client-independent check that two sessions on
different profiles do not share cookies or files; that check passes on
mac-smolvm. A second controller being refused is covered by bctl's Rust
tests, not by this suite.

## Known failing or unsupported cases

- **Plain-HTTP downloads.** Helium holds them for a person to confirm unless a
  client sets `Browser.setDownloadBehavior` to `allow` (see [files](files.md)).
- **agent-browser's download options.** Over `--cdp` it ignores
  `AGENT_BROWSER_DOWNLOAD_PATH` and refuses its own `download` command.
- **agent-browser time limit.** It gives `open` 30 s in total, including a
  provider launch, so a session that takes longer to boot fails the launch
  even though the plugin's own limit is 60 s.
- **No files directory on Nanos.** Nanos sessions have none.
- **Provider launch options.** The provider plugin cannot honour
  `userAgent` or `colorScheme`, and refuses them.

Timings for these tuples are in [performance](performance.md#external-clients).
