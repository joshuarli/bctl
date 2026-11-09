# WebAudio realtime threads and the musl SIGSYS kill on Helium r5

Outcome: **not reproduced**. Three AudioContext pages, each rendering an
oscillator through an AudioWorklet, and one contended page ran together in a
2-vCPU helium VM for 65 s twice with no `Target.targetCrashed`, no
`Target.targetDestroyed`, and monotonically advancing `AudioContext.currentTime`.
No sandbox patch is proposed.

## What ran

The test is `crates/bctl/tests/media.rs`,
`web_audio_worklet_threads_survive_a_minute_of_contended_playback`.

```
cd .work/tmp/run
TMPDIR=/Users/josh/d/bctl/.work/wt/webaudio/.work/tmp \
  cargo test -p bctl --test media -- --ignored --nocapture
```

The command was run three times and passed three times. The exact required
form without `--nocapture` also exited 0:

```
cd .work/tmp/run
TMPDIR=/Users/josh/d/bctl/.work/wt/webaudio/.work/tmp \
  cargo test -p bctl --test media -- --ignored
# exit 0; test result: ok. 1 passed; 0 failed; ... finished in 69.04s
```

The two `--nocapture` runs printed the measurements. The second run's exit
status was captured as 0; the first was piped through `tail`, which hides
cargo's status, and printed the same pass line:

```
# second run (exit 0)
web audio: 13 rounds in 65.097117875s, 65.1s / 65.1s / 65.1s of audio time, 24320 quanta / 24320 quanta / 24320 quanta
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 69.06s

# first run (exit status not captured)
web audio: 13 rounds in 65.103745167s, 65.1s / 65.1s / 65.1s of audio time, 24320 quanta / 24320 quanta / 24320 quanta
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 69.04s
```

`cargo test -p bctl --test media --no-run` in the same directory exited 0.

`.work/tmp/run` is a scratch mirror of this worktree (same sources) plus
`web/dist` copied from `/Users/josh/d/bctl/web/dist`. The lane worktree itself
cannot compile `bctl`: `crates/bctl/build.rs:27-29` panics because `web/dist`
does not exist, and the writable-path boundary does not include `web/`.

The run:

- boots the default image (`BCTL_TEST_IMAGE` unset), whose receipt is
  `helium-0.20.3.1-r5-linux-aarch64-musl.tar.xz`, SHA-256 `35858f46…c44`;
- opens four page targets in one session: three `/tone` and one `/contended`;
- each `/tone` page creates a 48 kHz `AudioContext`, adds an AudioWorklet
  module from a Blob, connects `OscillatorNode(220 Hz)` through an
  `AudioWorkletNode` to `context.destination`, resumes the context under a CDP
  user gesture, and reports `currentTime` and a worklet quantum counter;
- the `/contended` page runs three Web Workers, each burning 30 ms in a loop
  with only `setTimeout(..., 0)` between burns;
- subscribes to `Target.targetCrashed` and `Target.targetDestroyed` after
  `Target.setDiscoverTargets`;
- every 5 s, for 13 rounds over 65 s, asserts the three audio pages are still
  `running`, each `currentTime` is strictly greater than the previous round,
  each worklet counter is strictly greater, and the contended page still has
  three workers;
- asserts no crash or destruction of the four targets, at least 20 s of audio
  time advanced per context, all four targets remain in `Target.getTargets`,
  and each page still answers CDP.

The pages are delivered through `Fetch.fulfillRequest` on
`https://audio.test/...`. `AudioWorklet` is `[SecureContext]`
(`third_party/blink/renderer/modules/webaudio/base_audio_context.idl:26`), and
`http://100.96.0.1` is not potentially trustworthy: only https/wss, loopback
addresses, `.localhost`, file, and allowlisted origins are
(`net/base/is_potentially_trustworthy.cc:306-340`,
`net/base/url_util.cc:468-477`). Interception also avoids needing a TLS
server, a certificate on the guest, or a CDP certificate-error override.

## Why the P2b blocked-signal kill is not reached

### The audio thread reaches libc `sched_setscheduler`, not `pthread_setschedparam`

`RealtimeAudioWorkletThread` sets `base::ThreadType::kRealtimeAudio`
(`third_party/blink/renderer/modules/webaudio/realtime_audio_worklet_thread.cc:58-69`);
the feature is on by default
(`third_party/blink/common/features.cc:176-177`). The renderer's output callback
thread is also created with that type
(`media/audio/audio_output_device.cc:433-435`).

Creation sets the priority inside the new thread
(`base/threading/platform_thread_posix.cc:95` →
`base/threading/platform_thread.cc:49-50`, `:120-133`). On Linux that reaches
`SetThreadTypeLinux` (`base/threading/platform_thread_linux.cc:263-285`), which:

- replaces the target with 0 when the thread sets its own priority
  (`:271-274`, "Some scheduler syscalls require thread ID of 0 for current
  thread");
- calls the libc function `sched_setscheduler(0, SCHED_RR, …)` (`:277-278`);
- on failure falls back to `SetThreadNiceFromType`, which calls
  `setpriority(PRIO_PROCESS, 0, -10)` (`:285`;
  `base/threading/platform_thread_internal_posix.cc:51-67`, with `who == 0`
  chosen at `:58-59` because "0 is preferred to the current thread id ... but
  makes sandboxing easier").

There is no `pthread_setschedparam` in that path.

### musl 1.2.6 makes `sched_setscheduler` a no-syscall stub

musl 1.2.6 (`https://musl.libc.org/releases/musl-1.2.6.tar.gz`, sha256
`d585fd3b…512a`):

```
src/sched/sched_setscheduler.c:5-8
int sched_setscheduler(pid_t pid, int sched, const struct sched_param *param)
{
	return __syscall_ret(-ENOSYS);
}
```

So Chromium's call returns `-ENOSYS` without entering the kernel. There is no
seccomp evaluation, no SIGSYS, and no handler. The thread takes the
`setpriority` fallback instead, which the baseline policy allows when `who` is
0 or the process pid (`sandbox/linux/seccomp-bpf-helpers/baseline_policy.cc:246-247`,
`syscall_parameters_restrictions.cc:392-398`).

The hazardous function is signal-blocking and does issue the syscall:

```
src/thread/pthread_setschedparam.c:4-14
	__block_app_sigs(&set);
	LOCK(t->killlock);
	r = !t->tid ? ESRCH : -__syscall(SYS_sched_setscheduler, t->tid, policy, param);
	UNLOCK(t->killlock);
	__restore_sigs(&set);
```

`__block_app_sigs` blocks application signals, so a seccomp SIGSYS delivery from
`SYS_sched_setscheduler` (119 on arm64) has the same fatal outcome as P2b's
`pthread_getschedparam`. `pthread_setschedprio.c:4-14` has the same shape with
`SYS_sched_setparam`.

### The shipped binary confirms the stub and the absent caller

`/opt/browser/helium` in `bctl/helium:latest` (the same r5 rootfs as
`.work/images/helium.tar`) is dynamically linked:

```
NEEDED  libc.musl-aarch64.so.1
```

The only scheduling-priority symbols it references are `sched_setscheduler` and
`setpriority`; it has no reference to `pthread_setschedparam` or
`pthread_setschedprio`:

```
U sched_setscheduler
U setpriority
```

`/lib/libc.musl-aarch64.so.1` is a symlink to the image's
`/lib/ld-musl-aarch64.so.1`, which reports `musl libc (aarch64)` version
`1.2.6`. Disassembled from that file (the loader's static symbols are stripped,
so the helper labels shown by the tool are unrelated; the annotations in the
right column come from musl 1.2.6's source):

```
0000000000051020 <sched_setscheduler>:
  51024: mov  x0, #0xffffffffffffffda   ; -38 = -ENOSYS
  5102c: bl   22e68                     ; __syscall_ret
  51034: ret

0000000000064540 <pthread_setschedparam>:
  64568: bl   51ca0                     ; __block_app_sigs
  64574: ldr  w0, [x19, #32]            ; t->tid
  6457c: cbz  w0, 64598                 ; tid == 0 -> ESRCH
  64580: mov  x8, #0x77                 ; 119 = SYS_sched_setscheduler
  64590: svc  #0x0
  6459c: bl   60924                     ; unlock
  645a4: bl   51cc0                     ; __restore_sigs
```

So the only scheduling-priority call the audio threads make is resolved to a
function that returns `-ENOSYS` without a `svc`, while the function that would
trap is not referenced by the browser at all.

### No WebAudio caller of the hazardous functions

A recursive source scan for `pthread_setschedparam` in the Chromium tree found
callers only in WebRTC's `rtc_base/platform_thread.cc:97` (WebRTC's own platform
thread priority), PipeWire's `module-rt.c`/`thread.c`, and SwiftShader's
bundled LLVM 10.0 and 16.0
(`third_party/swiftshader/third_party/llvm-10.0/.../Threading.inc`,
`third_party/swiftshader/third_party/llvm-16.0/.../Threading.inc`).
None is on the WebAudio path: WebAudio does not use `rtc::PlatformThread`,
PipeWire is not built (the arm64 `helium-wayland-release` profile forces
`use_alsa = false` while `use_pulseaudio` is false;
`/Users/josh/repos/laputa-systems/chromium-build/scripts/generate-gn.py:67-69`,
`config/profiles/helium-wayland-release.gn:55-56`), and SwiftShader is a GPU
library. The shipped `helium` binary's symbol table agrees: no reference to
`pthread_setschedparam` or `pthread_setschedprio`.

Abseil is the known `pthread_getschedparam` caller
(`third_party/abseil-cpp/absl/synchronization/mutex.cc:958-964`), which r5
already allows; it has no `pthread_setschedparam` call.

### The r5 policy is consistent with the observation

`baseline_policy.cc:250-253` routes the scheduling syscalls through
`RestrictSchedTarget`. In the r5 tree
(`syscall_parameters_restrictions.cc:400-430`, from chromium-build `4275db8`):

- `sched_getparam` and `sched_getscheduler` are `Allow()` for any target;
- `sched_setscheduler` is allowed for pid 0 or the process pid and otherwise
  `RewriteSchedSIGSYS()`;
- the handler rewrites the target to 0 only when it equals `sys_gettid()`, and
  otherwise crashes (`sigsys_handlers.cc:348-375`).

Chromium passes 0, so even a real `sched_setscheduler` syscall would be
allowed. The blocked-signal kill is specific to musl's pthread wrappers, which
this path does not use.

## Commands behind the binary evidence

The Chromium source was read from the `helium-0.20.3.1-work-arm64` volume
(read-only) and the checkout at chromium-build `e9809cb`, which contains
`4275db8`. The shipped symbols and loader were inspected from
`bctl/helium:latest` and the same volume:

```
docker run --rm -v helium-0.20.3.1-work-arm64:/work:ro alpine:3.22 sh -c \
  'apk add -q binutils; readelf -d /work/stage/helium-wayland-release/helium \
     | grep -E "NEEDED"; \
   nm /work/stage/helium-wayland-release/helium \
     | grep -E "sched_setscheduler|pthread_setschedparam|pthread_setschedprio|setpriority"'
# NEEDED  libc.musl-aarch64.so.1
# U sched_setscheduler
# U setpriority

id=$(docker create bctl/helium:latest)
docker cp "$id":/lib/ld-musl-aarch64.so.1 ld-musl-image
docker rm "$id"
docker run --rm -v "$PWD:/w" alpine:3.22 sh -c \
  'apk add -q binutils; objdump -d --disassemble=sched_setscheduler /w/ld-musl-image; \
   objdump -d --disassemble=pthread_setschedparam /w/ld-musl-image'
```

Both commands exited 0. The loader reports `musl libc (aarch64)` version
`1.2.6`; `/lib/libc.musl-aarch64.so.1` in the image is a symlink to it.

### musl's `pthread_create` is the one other route, and it is not reached

musl 1.2.6's `pthread_create` makes the same blocked-signal `sched_setscheduler`
call on the new thread's tid when the attribute asks for an explicit scheduler
(`src/thread/pthread_create.c:344`, `:363-364`). Only
`pthread_attr_setinheritsched` sets that flag (`pthread_attr_setinheritsched.c:7`);
`pthread_attr_setschedpolicy`, which `helium` imports, does not. Neither `helium`
nor any library in r5's `lib/` imports `pthread_attr_setinheritsched` (checked
with `nm -D --undefined-only` over the extracted archive).

### Realtime audio priority is never applied

Because musl's `sched_setscheduler` returns `ENOSYS` without making a syscall,
Chromium's request for `SCHED_RR` on audio threads always fails. The thread then
falls back to `setpriority(PRIO_PROCESS, 0, -10)`, which an unprivileged browser
user cannot get either. The audio threads run at normal priority. Nothing in the
test showed underruns, but a loaded guest could.

## What would change the conclusion

A future Chromium or linked library that calls `pthread_setschedparam`,
`pthread_setschedprio`, or `pthread_attr_setinheritsched(PTHREAD_EXPLICIT_SCHED)`
from a sandboxed thread would restore the P2b hazard: the
wrapper blocks SIGSYS and passes the thread's nonzero tid, which
`RestrictSchedTarget` rewrites through SIGSYS. The policy cannot distinguish
"the calling thread's tid" from another tid in BPF, but the existing handler
already rewrites it to 0; the failure is in musl's signal blocking, not in the
policy. A defensive sandbox change would be to allow `sched_setscheduler` for
the current thread, but that is not justified by any observed call and is not
proposed here.

## Unverified

- No crash was reproduced, so there is no SIGSYS syscall number from a trap and
  no gdb backtrace.
- No live syscall trace of the VM's audio threads was taken (`strace`/gdb were
  not added to the image). The "not reached" conclusion rests on the shipped
  binary's dynamic symbols, the shipped loader's disassembly, the musl 1.2.6
  sources, and the Chromium sources above, not on tracing a running thread.
- The test observes `currentTime` and worklet quantum messages, not the
  scheduler policy of any thread.
- Three 65 s runs on this Mac, with other lanes' VMs running. A longer soak or
  a deliberately tighter CPU budget was not run.
