# Handoff: crashpad rejects Nanos's recvmsg control data

For the session that owns G2.1 (`nanos/src/unix/socket.c`). This is the
current blocker for starting Helium on Nanos.

## Symptom

`dev/test.sh helium` on `main` (`a0890ba8` or later): the browser exits with
status 191 about 5 s after start. Its log (printed to the serial log between
`GUEST_LOG_BEGIN`/`GUEST_LOG_END`) ends with:

```
crashpad/util/linux/socket.cc:157] unhandled cmsg 0, 0
crashpad/util/linux/socket.cc:157] unhandled cmsg 0, 0
```

The run's directory is printed as `log:`; read its `serial.txt`.

## What crashpad does

`UnixCredentialSocket::RecvMsg` (crashpad `util/linux/socket.cc:100-175`, in
the Helium source volume `hydrogen-helium-0.20.3.1-src-amd64`):

1. The control buffer holds
   `CMSG_SPACE(sizeof(ucred)) + CMSG_SPACE(sizeof(int) * kMaxSendRecvMsgFDs)`
   bytes. It is uninitialized, and `msg_controllen` is set to its full size.
2. It calls `recvmsg(fd, &msg, 0)` on a socket that has `SO_PASSCRED` set.
3. It walks every header with `CMSG_FIRSTHDR`/`CMSG_NXTHDR`.
   - `SOL_SOCKET`/`SCM_RIGHTS` and `SOL_SOCKET`/`SCM_CREDENTIALS` are
     accepted.
   - Any other header is logged as `unhandled cmsg` and fails the receive.
4. It also fails on `MSG_TRUNC`/`MSG_CTRUNC`, or on a non-empty `msg_name`.

`cmsg 0, 0` means the walk reached a header that is all zeros.

## Likely cause (unverified)

On return, Linux sets `msg_controllen` to exactly the bytes it wrote. When
`SO_PASSCRED` is on, it always includes one `SCM_CREDENTIALS` header, even if
the sender attached none. If Nanos leaves `msg_controllen` larger than what
it wrote, `CMSG_NXTHDR` walks into the unwritten rest of the buffer.
Writing a zeroed or short header would have the same effect. Check the
receive path for both.

## Acceptance

1. Add a fixture case that mirrors crashpad's receive.
   - Use a stream socketpair with `SO_PASSCRED` on the receiver.
   - Fill the control buffer with a non-zero pattern before `recvmsg`.
   - Cover three sends: plain data, data with `SCM_RIGHTS`, and data with
     explicit `SCM_CREDENTIALS`.
   - Every header the walk visits must be `SOL_SOCKET` with type
     `SCM_RIGHTS` or `SCM_CREDENTIALS`.
   - `msg_controllen` must equal the sum of `CMSG_SPACE` of the headers
     written.
   - No `MSG_CTRUNC`.

   The case must pass natively (Linux is the reference) and on Nanos at
   `--smp 1` and `--smp 2`. It must fail on Nanos before the fix.
2. `dev/test.sh harness debug probe upstream external` prints `test: ok`.
3. `dev/test.sh helium` no longer logs `unhandled cmsg`. Report how far the
   browser gets: the `helium_browser_up` record and the log tail.

Commit on a branch, as the follow-ups were; hydrogen's coordinator rebases,
reruns the suite and pushes.
