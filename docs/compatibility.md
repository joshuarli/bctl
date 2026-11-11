# External client compatibility

What each pinned client is verified to do against bctl, per host and
runtime. A row is supported only on the tuples where `tests/interop/run.sh`
passed it. Anything not listed is unverified.

## Verified tuples

| Tuple | Host | Runtime | Image | Checks |
|-------|------|---------|-------|--------|
| mac-smolvm | macOS 27.0.1, Apple M5 Max (arm64) | smolvm 1.34.0 | `helium` sha256:a91a0ab8… | 113 of 113 |
| mac-nanos | macOS 27.0.1, Apple M5 Max (arm64) | Nanos on libkrun, Hydrogen `bbeea356` | `nanos-helium` sha256:f6db6190… | 105 of 105, twice |
| linux-nanos | Alpine Linux edge (7.2.9), AMD Ryzen 9 9950X (x86_64) | Nanos on QEMU 11.1.2 with KVM, vhost-device-vsock 0.3.0, virtiofsd 1.14.0, Hydrogen `6dc7d43e` | `nanos-helium` sha256:b8239e70… | 113 of 113 in 2 of 4 runs; see [intermittent failures](#known-failing-or-unsupported-cases) |

Every tuple uses Helium 0.20.3.1 and the same client pins:

- browser-use 1.0.0 (`070be98`), with browser-harness 0.1.13
- agent-browser 0.42.0 (`e751995`)

The linux-nanos receipts are from bctl `d0da931`; one of its two runs there
passed. The other two runs were on the same code before it was committed.
mac-nanos predates the files directory on Nanos and was verified on an older
Hydrogen; it needs a rerun at the current pin.

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
| Upload a host file | ✓ through the page | – | ✓ | ? | ✓ |
| Upload from the files directory by guest path | – | ✓ | ✓ | ? | ✓ |
| Download into the files directory | ✓ | ? | ✓ | ? | ✓ |
| `--provider bctl`: owned launch, close, reclaim after owner death | – | ✓ | ✓ | ✓ | ✓ |
| `--provider bctl`: borrowed lease, close revokes only its URL | – | ✓ | ✓ | ✓ | ✓ |
| `--provider bctl`: refuses other engines, `headed` on Nanos | – | ✓ | ✓ | ✓ | ✓ |

agent-browser downloads have no check. The mac-nanos `?` cells are file
checks the suite skipped on Nanos when that tuple ran. The
client-independent check that two sessions on different profiles do not
share cookies or files passes on mac-smolvm and linux-nanos. A second
controller being refused is covered by bctl's Rust tests, not by this suite.

## Known failing or unsupported cases

- **Plain-HTTP downloads.** Helium holds them for a person to confirm unless a
  client sets `Browser.setDownloadBehavior` to `allow` (see [files](files.md)).
- **agent-browser's download options.** Over `--cdp` it ignores
  `AGENT_BROWSER_DOWNLOAD_PATH` and refuses its own `download` command.
- **agent-browser time limit.** It gives `open` 30 s in total, including a
  provider launch, so a session that takes longer to boot fails the launch
  even though the plugin's own limit is 60 s.
- **Intermittent failures on linux-nanos.** Two of four runs failed one or
  two checks each:
  - browser-use's `ultrafast` cell had a `Page.navigate` time out after
    20 s, so it saw no popup.
  - `bctl down` of the suite's second session failed with an empty reply to
    the guest's shutdown request, so `bctl rm` kept its files directory. The
    same empty reply came once in 84 up/down cycles outside the suite. No
    failing run kept the guest console, so the cause is not known.

  The "guest stall" recorded here before was not a stall. `bctl run` hashed
  the 2 GiB image on every run, which took 97 s in a debug build, and that
  was fixed in `a11b620`.
- **Provider launch options.** The provider plugin cannot honour
  `userAgent` or `colorScheme`, and refuses them.

Timings for these tuples are in [performance](performance.md#external-clients).
