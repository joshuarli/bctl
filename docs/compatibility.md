# External client compatibility

What each pinned client is verified to do against bctl, per host and
runtime. A row is supported only on the tuples where `tests/interop/run.sh`
passed it. Anything not listed is unverified.

## Verified tuples

| Tuple | Host | Runtime | Image | Checks |
|-------|------|---------|-------|--------|
| mac-smolvm | macOS 27.0.1, Apple M5 Max (arm64) | smolvm 1.34.0 | `helium` sha256:a91a0ab8… | 113 of 113 |
| mac-nanos | macOS 27.0.1, Apple M5 Max (arm64) | Nanos on libkrun, Hydrogen `bbeea356` | `nanos-helium` sha256:f6db6190… | 105 of 105, twice |
| linux-nanos | Alpine Linux edge (7.2.9), AMD Ryzen 9 9950X (x86_64) | Nanos on QEMU 11.1.2 with KVM, vhost-device-vsock 0.3.0, Hydrogen `df949087` | `nanos-helium` sha256:f25abf2f… | 105 of 105 in 2 of 6 runs; see [the guest stall](#known-failing-or-unsupported-cases) |

Every tuple uses Helium 0.20.3.1 and the same client pins:

- browser-use 1.0.0 (`070be98`), with browser-harness 0.1.13
- agent-browser 0.42.0 (`e751995`)

The linux-nanos passes were on bctl `c9db778`, from copies of `run.sh` that
differ only in keeping the daemon's logs. Its receipts came from the same
revision's `run.sh`, in a run that passed 102 of 105.

Reproduce a tuple:

```sh
BCTL_RUNTIME=smolvm sh tests/interop/run.sh
BCTL_RUNTIME=nanos sh tests/interop/run.sh
```

## Features

✓ passed, – not applicable, ? not run.

| Feature | browser-use | agent-browser | mac-smolvm | mac-nanos | linux-nanos |
|---------|:-----------:|:-------------:|:----------:|:---------:|:-----------:|
| Attach over a `bctl cdp-url` capability URL | ✓ | ✓ | ✓ | ✓ | ✓ |
| Navigate, fill, click; the fixture sees the submission | ✓ | ✓ | ✓ | ✓ | ✓ |
| Accessibility tree, screenshot | ✓ | ✓ | ✓ | ✓ | ✓ |
| Dialogs, popups, same-site and cross-site frames | ✓ | ✓ | ✓ | ✓ | ✓ |
| 10 attach and detach cycles, no socket left open | ✓ | ✓ | ✓ | ✓ | ✓ |
| browser-use's domain guard and its second CDP socket | ✓ | – | ✓ | ✓ | ✓ |
| Revocation closes the client's sockets | ✓ | ✓ | ✓ | ✓ | ✓ |
| Run as a `bctl run` job: result, record, cancel, no retry | ✓ | ✓ | ✓ | ✓ | ✓ |
| Upload a host file | ✓ through the page | – | ✓ | ? | ? |
| Upload from the files directory by guest path | – | ✓ | ✓ | – no files directory | – no files directory |
| Download into the files directory | ✓ | ? | ✓ | – no files directory | – no files directory |
| `--provider bctl`: owned launch, close, reclaim after owner death | – | ✓ | ✓ | ✓ | ✓ |
| `--provider bctl`: borrowed lease, close revokes only its URL | – | ✓ | ✓ | ✓ | ✓ |
| `--provider bctl`: refuses other engines, `headed` on Nanos | – | ✓ | ✓ | ✓ | ✓ |

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
- **Intermittent guest browser stall on linux-nanos.** 4 of 6 runs on
  Hydrogen `df949087` failed. In three of them, Helium in the guest stopped
  answering CDP partway through. The VM stayed up, but the gateway answered
  502, and `bctl run` waited the suite's full 120 s. The guest's own
  `Browser.close` timed out with `EAGAIN`, and Helium ignored SIGTERM until
  it was killed. In two runs, one of them a run without the stall, the owned
  provider session outlived its `close`. An xsh test campaign shared the
  host, with load averages between 4 and 25. Load does not separate passes
  from failures: one failure started at 6.6, one pass at 17.2. Hydrogen
  `bea1e222` passed its one run. The Mac's one owner-death relaunch timeout
  hit the same `EAGAIN`.
- **Provider launch options.** The provider plugin cannot honour
  `userAgent` or `colorScheme`, and refuses them.

Timings for these tuples are in [performance](performance.md#external-clients).
