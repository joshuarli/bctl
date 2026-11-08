# Run seatd

Seatd is a foreground Linux daemon for one seat, `seat0`. Run it under an
external supervisor. Seatd does not fork, authenticate users, manage logins,
or manage power or supervision policy.

## Set up socket access

Seatd listens on `/run/seatd.sock` by default. Set `SEATD_SOCK` to use another
path. The socket directory must exist and be controlled by the administrator
or supervisor.

Use `-u user` and `-g group` to set socket ownership. Supplying either option
sets the socket mode to `0770`. Without either option, seatd still restricts
new sockets to `0770` (restrictive umask around bind plus explicit chmod
before listen). `-u` and `-g` cannot be combined with `-z`.

Socket access grants access to the daemon's seat operations. Seatd records
peer credentials but does not use them for authorization. Every connected
peer can request any supported operation, including opening supported device
classes while its session is active. Protect the socket and its directory as
the trust boundary.

## Start under supervision

To run without VT binding:

```sh
SEATD_VTBOUND=0 seatd -g seat -l info
```

The daemon runs in the foreground. It logs at `info` by default. The `-n fd`
option writes one newline and closes the supplied descriptor after the socket
is ready. This supports supervisors that use the s6 readiness protocol.

At startup, seatd probes an existing socket at the configured path: a live
daemon is never stolen from and a non-socket file is never removed; only
a stale socket left by a dead daemon is reclaimed (concurrent startups
are serialized with a lock file). Use `-z` to refuse startup when
anything already occupies the path. `-z` is mutually exclusive with
`-u` and `-g`.

## Select VT behavior

`SEATD_VTBOUND` enables VT binding only when its value is exactly `1`. An
unset variable enables VT binding. Values such as `0`, `true`, or an empty
string disable it.

VT-bound operation passed switching, release/acquire, close restoration, and
SIGTERM restoration in an x86_64 KVM guest running Alpine Linux 6.6.142-0-virt.
Virtio DRM master handoff, evdev revocation/reopen, and dwl/foot rendering and
keyboard input passed in that guest. See [the reproduction recipe](../harness/VM.md).
These results do not qualify other kernels, physical devices, hidraw revocation,
or ARM hardware. Never run the proof on host console, GPU, or input devices.

## Device and protocol scope

Seatd accepts Linux device paths with these canonical prefixes:

- `/dev/dri/` for DRM devices.
- `/dev/input/event` for evdev devices.
- `/dev/hidraw` for hidraw devices.

It resolves paths before classifying them and checks the opened descriptor's
identity and character-device type. This is a single-seat implementation; it
always creates `seat0`.

The daemon retains the libseat seatd wire protocol. The four-byte header and
integer fields use native byte order. Unix socket descriptor passing uses
`SCM_RIGHTS` as before; the Rust port does not change the FD wire format.

Per connection, the input and output byte buffers are each 256 bytes, and the
received and queued descriptor buffers each hold up to 64 file descriptors.
The daemon supports up to 128 devices for one client. Device path fields are
limited to 256 bytes, including the terminating NUL byte.

## Revocation failure semantics

Device revocation follows the fail-open handoff rule: a failed
revoke is logged and the session transition continues, so a disable
notification does not prove the old client's descriptors stopped
working. The daemon never clears the device's active flag on a failed
revoke, and the failure is observable to the operator through the
error log. Treat any session handoff that follows a revocation error
as suspect: restart the affected clients and have them close every
reference and reopen their devices, per [recovery](recovery.md).
`session_revoke_failure_across_full_switch` pins this contract.

## Recover after failure

For SIGINT or SIGTERM, seatd stops accepting clients, cleans up sessions,
attempts device revocation, closes its descriptors, and exits. See
[recovery instructions](recovery.md) for stale sockets, SIGKILL, and client
restarts. A SIGKILL cannot run cleanup. A device descriptor already passed to
a client may remain open after the daemon exits.

## See also

- [Command-line reference](../man/seatd.1.scd)
- [Recovery after failure](recovery.md)
- [Validation status](validation.md)
