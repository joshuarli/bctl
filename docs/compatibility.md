# External client compatibility

What each pinned client is verified to do against bctl, per host and
runtime. A row is supported only on the tuples where `tests/interop/run.sh`
passed it. Anything not listed is unverified.

## Verified tuples

| Tuple | Host | Runtime | Image | Checks |
|-------|------|---------|-------|--------|
| mac-smolvm | macOS 27.0.1, Apple M5 Max (arm64) | smolvm 1.34.1 (upstream `59bdcf62`, 13 patches) | `helium` sha256:a889b1d2… (tinyland `5c5da46`) | 113 of 113 in each of two runs at bctl `afe91b6` |
| mac-nanos | macOS 27.0.1, Apple M5 Max (arm64) | Nanos on libkrun, Hydrogen `5fe102e8` | `nanos-helium` sha256:ed145673… | 113 of 113 in each of two runs |
| linux-nanos | Alpine Linux edge (7.2.9), AMD Ryzen 9 9950X (x86_64) | Nanos on QEMU 11.1.2 with KVM, vhost-device-vsock 0.3.0, virtiofsd 1.14.0, Hydrogen `44f4b61f` | `nanos-helium` sha256:9113ccc3… | 113 of 113 in 2 of 2 runs; see [intermittent failures](#known-failing-or-unsupported-cases) |
| linux-smolvm | Alpine Linux edge (7.2.9), AMD Ryzen 9 9950X (x86_64) | smolvm 1.34.1 (upstream `59bdcf62`, 13 patches) with KVM, headed 2D display, the default runtime | `helium` sha256:c21aaf05… (tinyland `5c5da46`) | 113 of 113 in 2 of 2 runs at bctl `9ead6b5` |

Every tuple uses Helium 0.20.3.1 and the same client pins:

- browser-use 1.0.0 (`070be98`), with browser-harness 0.1.13
- agent-browser 0.42.0 (`e751995`)

The linux-nanos receipts are from bctl `3b134a4` with the Hydrogen pin
above. The last three runs also include this row's daemon restart fix.
The mac-nanos receipts use the final ARM pin from bctl `afe91b6`.
Both runs passed. The first run preceded the pin's commit.
The refreshed macOS receipts and rebuilt artifact identities are in
the [macOS qualification results](handoffs/2026-11-12-macos-results.md).

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
| Upload a host file | ✓ through the page | – | ✓ | ✓ | ✓ |
| Upload from the files directory by guest path | – | ✓ | ✓ | ✓ | ✓ |
| Download into the files directory | ✓ | ? | ✓ | ✓ | ✓ |
| `--provider bctl`: owned launch, close, reclaim after owner death | – | ✓ | ✓ | ✓ | ✓ |
| `--provider bctl`: borrowed lease, close revokes only its URL | – | ✓ | ✓ | ✓ | ✓ |
| `--provider bctl`: refuses other engines, `headed` on Nanos | – | ✓ | ✓ | ✓ | ✓ |

agent-browser downloads have no check. The
client-independent check that two sessions on different profiles do not
share cookies or files passes on mac-smolvm, mac-nanos, and linux-nanos.
A second controller being refused is covered by bctl's Rust tests, not by
this suite.

## Known failing or unsupported cases

- **Plain-HTTP downloads.** Helium holds them for a person to confirm unless a
  client sets `Browser.setDownloadBehavior` to `allow` (see [files](files.md)).
- **agent-browser's download options.** Over `--cdp` it ignores
  `AGENT_BROWSER_DOWNLOAD_PATH` and refuses its own `download` command.
- **agent-browser time limit.** It gives `open` 30 s in total, including a
  provider launch, so a session that takes longer to boot fails the launch
  even though the plugin's own limit is 60 s.
- **Intermittent failures on linux-nanos.** Two of six runs at Hydrogen
  `47bc0255` stopped in the `job/daemon` check. Right after `kill -9` of the
  daemon, `bctl runs <id>` got `Connection reset by peer` from the dead
  daemon's socket, and the CLI gave up instead of starting a new daemon.
  One later linux-smolvm run got the reset while the new daemon already
  held the lock. On macOS, closing the same unread socket produces EOF.
  The CLI now probes with a side-effect-free `ping` before sending the
  operation. It retries dropped pings until its 15 s start deadline,
  whether or not a daemon holds the lock. An operation that loses its
  answer is retried only when transport failure proves it was not delivered.

  Outside the suite, four parallel loops of `bctl up` and `bctl down` used to
  fail about once per 400 cycles, when the guest kernel crashed on a null task
  in its scheduler queue. Hydrogen `0523dbbc` moved the scheduler's run queue
  to a locked heap, and the next 2,400 cycles had no failure.
  [Hydrogen's campaign report](../../hydrogen/docs/campaign-report.md) lists
  the kernel faults fixed so far.
- **Provider launch options.** The provider plugin cannot honour
  `userAgent` or `colorScheme`, and refuses them.

Earlier client timing measurements are in [performance](performance.md#external-clients).
