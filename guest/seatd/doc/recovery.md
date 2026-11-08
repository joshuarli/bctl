# Recover after a daemon or client failure

## Graceful shutdown

Send SIGINT or SIGTERM. Seatd stops admission, removes sessions, attempts device
revocation, closes its descriptors, and exits. Cleanup errors appear in stderr.
It unlinks the socket only when its device and inode still match the socket
captured at startup. A replacement daemon's socket remains available.

Use a socket directory writable only by the supervisor or administrator.
Identity checking does not make unlink atomic against a hostile writer in
that directory. Startup replaces an existing socket unless `-z` is supplied.
Use `-z` when a second instance must refuse to replace the first.

## Daemon crash or SIGKILL

SIGKILL performs no userspace cleanup and leaves the socket path behind.
The kernel closes the daemon's descriptors. Descriptor copies already passed
to clients remain open. Do not assume closing the daemon's copy revokes DRM
master or input access. No crash-time revoke ioctl runs.

Recover in this order:

1. Stop every client of the failed daemon and close its device descriptors.
2. Start the replacement daemon on the same socket path.
3. Restart clients so they reconnect and reopen every device.

A normal start reclaims only a stale socket left by a dead daemon; it
refuses to steal the pathname from a live daemon and never removes a
non-socket file. With `-z`, any existing socket or file at the path
refuses startup; remove a stale socket from the
administrator-controlled directory before restarting with `-z`.

Input descriptors successfully revoked before the crash remain revoked.
Clients must close every reference and reopen evdev and hidraw devices after
reactivation. There is no transparent recovery across daemon death.

## Client crash

Seatd removes a disconnected client's session, attempts device revocation,
closes daemon-held device descriptors, and activates a waiting session.
Revocation failures are logged. Closing a daemon descriptor does not guarantee
that a descriptor retained by another process has lost its authority.

## VT-bound operation

VT switching and graceful restoration passed the disposable-guest proof
documented in [VM qualification](../harness/VM.md). A crash does not guarantee restoration of keyboard, display, or VT modes.
Use the VM's separate management console to recover its console state before
restarting clients. Do not apply VM qualification steps to the host console.
