# Helium 0.20.3.1 segfaults at startup when `$HOME` does not exist

Root cause, reproduction, symbolized backtrace, and proposed chromium-build
Helium patch.

## Conclusion

- Reproduced on `linux/arm64` (OrbStack, aarch64): Helium r5 started as uid
  1000 with `HOME=/nonexistent` and a writable `--user-data-dir=/profile`
  dies with **exit 139 (SIGSEGV)**, `si_code=SEGV_MAPERR`,
  `si_addr=0x2620` (9760).
- The fault is an **unguarded null dereference in Crashpad**, inlined into
  `ChromeMainDelegate::PreSandboxStartup`:
  `CrashpadClient::SetFirstChanceExceptionHandler` does
  `DCHECK(SignalHandler::Get())` and then
  `SignalHandler::Get()->SetFirstChanceHandler(handler)`. In a release build
  the `DCHECK` is compiled out and `SignalHandler::Get()` returns the static
  `handler_`, which is `nullptr`; the store `first_chance_handler_ = handler`
  faults at `nullptr + 9760`.
- `handler_` is null because Helium's own
  `crash-reporter-privacy-tweaks.patch` inserts an early
  `return false` (`DisableCrashpadUploadsAtStartup`) **before**
  `CrashpadClient::StartHandler`, so `Install()` never runs. That call is
  reached because the crash database path is empty: `$HOME` cannot be
  created by uid 1000, so the `DIR_CRASH_DUMPS` path provider fails and
  `ChromeCrashReporterClient::GetCrashDumpLocation` leaves the out-path empty.
- The specific SIGSEGV is **introduced by Helium's patch, not by musl**. Upstream
  Chromium (glibc or musl) does not have that early return: forcing the gate
  to succeed in this same binary reaches `CHECK(client.StartHandler(...))`
  with the empty `--database`, the handler rejects it (`--database is
  required`), `StartHandler` fails, and `base`'s immediate-crash path executes
  `brk #0x0` (SIGTRAP). So upstream behaves differently (CHECK/`brk` instead
  of this null-write SEGV), and the difference is the Helium patch, not the
  libc.

## Inputs

- Archive:
  `helium-0.20.3.1-r5-linux-aarch64-musl.tar.xz`,
  SHA-256 `35858f463c1f2f6cf7a692c906188d0555c68b8a61a8f73774f5f37d34be0c44`
  (sidecar). Extracted to `.work/tmp/r5/` (exit 0).
- Runner: `bctl/helium:latest`,
  `sha256:e719d63466c86351322e211bffcf2b6afccca6cb5db229f9ea90c9faca844b14`.
  It supplies the archive's interpreter `/lib/ld-musl-aarch64.so.1`; the loader
  reports `musl libc (aarch64) Version 1.2.6`.
- Source (read-only): docker volume `helium-0.20.3.1-work-arm64`, mounted at
  `/src:ro`; Chromium source root `/src/src`, applied patches under
  `/src/metadata/patches/`.
- gdb: Alpine 3.22 `gdb` 15.2 (musl), run with `--cap-add SYS_PTRACE
  --security-opt seccomp=unconfined`.

## Reproduction

The image is a scratch rootfs, so the run supplies the two directories a normal
boot has: `/profile` (`--user-data-dir`, as `bctl-guest browser` passes) and
`/tmp` (Crashpad's process-singleton socket dir). `$HOME` is deliberately absent.

```sh
ARCH=$PWD/.work/tmp/r5/helium-0.20.3.1-r5-linux-aarch64-musl
docker run --rm --platform linux/arm64 --name hsegv-nonexist \
  --tmpfs /profile:exec,mode=777 --tmpfs /tmp:exec,mode=777 \
  -v "$ARCH":/b:ro --user 1000:1000 \
  -e HOME=/nonexistent -e LD_LIBRARY_PATH=/b/lib \
  -e FONTCONFIG_FILE=/b/fontconfig/fonts.conf -e FONTCONFIG_PATH=/b/fontconfig \
  --entrypoint /b/helium bctl/helium \
  --headless --no-sandbox --disable-gpu --user-data-dir=/profile \
  --no-first-run --no-default-browser-check --dump-dom about:blank
```

Exit status `139` (128+11, SIGSEGV), reproduced twice. Decisive stderr:

```
[1:1:1109/...:ERROR:src/third_party/crashpad/crashpad/util/file/filesystem_posix.cc:63] mkdir : No such file or directory (2)
```

`strace` (same command, `--entrypoint /usr/bin/strace ... -f -e
trace=mkdir,mkdirat,execve`) pins the two syscalls and the fault address:

```
mkdirat(AT_FDCWD, "/nonexistent", 0700) = -1 EACCES (Permission denied)
mkdirat(AT_FDCWD, "", 0700)             = -1 ENOENT (No such file or directory)
[8:8:...:ERROR:.../filesystem_posix.cc:63] mkdir : No such file or directory (2)
--- SIGSEGV {si_signo=SIGSEGV, si_code=SEGV_MAPERR, si_addr=0x2620} ---
+++ killed by SIGSEGV +++
```

`0x2620` is 9760: `nullptr` plus the offset of `SignalHandler::first_chance_handler_`.

Controls (same container, same flags):

| uid | HOME | result |
|---|---|---|
| 1000 | `/profile` (exists) | Crash Reports dirs created, `/b/helium_crashpad_handler` spawned, exit 0 |
| 0 | `/nonexistent` | root creates `/nonexistent`, handler spawned, exit 0 |
| 1000 | `/nonexistent` | `mkdirat("/nonexistent") = EACCES`, empty DB path, no handler spawn, exit 139 |

The uid is what matters because the directory `/nonexistent` sits directly under
`/`; uid 1000 may not create it, uid 0 may.

## Symbolized fault and backtrace

gdb 15.2, same environment:

```
Program received signal SIGSEGV, Segmentation fault.
0x0000aaaaad8a2f94 in ChromeMainDelegate::PreSandboxStartup() ()

info line *$pc
No line number information available for address 0x... <_ZN18ChromeMainDelegate17PreSandboxStartupEv+1584>

info symbol $x8
crash_reporter::(anonymous namespace)::FirstChanceHandlerHelper(int, siginfo_t*, ucontext*) in section .text

info symbol 0xaaaabb9d9b20
crashpad::(anonymous namespace)::SignalHandler::handler_ in section .bss
0xaaaabb9d9b20 <SignalHandler::handler_>:   0x0000000000000000

=> 0xaaaaad8a2f94 <ChromeMainDelegate::PreSandboxStartup()+1584>:  str x8, [x9, #9760]
   0xaaaaad8a2f98 <ChromeMainDelegate::PreSandboxStartup()+1588>:  mov x0, x19
   0xaaaaad8a2f9c <ChromeMainDelegate::PreSandboxStartup()+1592>:  bl crash_keys::SetCrashKeysFromCommandLine(...)
```

The binary has `.symtab` and `.eh_frame` but no `.debug_info`/`.debug_line`, and
gdb's `bt` loops on the inlined call. A manual `x29` frame walk gives the real
chain:

```
frame0 pc: ChromeMainDelegate::PreSandboxStartup() + 1584
frame0 lr: ChromeMainDelegate::PreSandboxStartup() + 1552
fp[0] lr: content::ContentMainRunnerImpl::Initialize(content::ContentMainParams) + 1844
fp[1] lr: content::ContentMain(content::ContentMainParams) + 812
fp[2] lr: ChromeMain + 696
fp[3] lr: libc_start_main_stage2 + 56 in /lib/ld-musl-aarch64.so.1
```

At the fault: `x8` = `FirstChanceHandlerHelper`, `x9` = `0`. The store is the
inlined `SignalHandler::SetFirstChanceHandler`:

```cpp
// third_party/crashpad/crashpad/client/crashpad_client_linux.cc
159  void SetFirstChanceHandler(CrashpadClient::FirstChanceHandler handler) {
160    first_chance_handler_ = handler;   // <-- store to nullptr + 9760
161  }
...
767  void CrashpadClient::SetFirstChanceExceptionHandler(
768      FirstChanceHandler handler) {
769    DCHECK(SignalHandler::Get());       // compiled out in release
770    SignalHandler::Get()->SetFirstChanceHandler(handler);
771  }
```

## Causal chain, file:line

1. `$HOME=/nonexistent`.
   `base::GetHomeDir()` returns `$HOME` when set:
   `base/files/file_util_posix.cc:1089-1092`.
2. `chrome::GetDefaultUserDataDirectory` builds
   `${XDG_CONFIG_HOME:-$HOME/.config}/net.imput.helium`:
   `chrome/common/chrome_paths_linux.cc:80-94`; the `$HOME` fallback is
   `base/nix/xdg_util.cc:44-54` (`PathService::Get(DIR_HOME)` at `:51`).
3. `chrome::PathProvider(DIR_CRASH_DUMPS)` appends `Crash Reports` and sets
   `create_dir = true`: `chrome/common/chrome_paths.cc:210-238`. The provider
   fails if it cannot create it:
   `chrome/common/chrome_paths.cc:538-540`. `base::CreateDirectory` first tries
   `mkdirat("/nonexistent")`, which is `EACCES` for uid 1000 (strace above).
4. `PathService::Get(chrome::DIR_CRASH_DUMPS, ...)` therefore returns false;
   `ChromeCrashReporterClient::GetCrashDumpLocation` returns that false and
   leaves the caller's `database_path` empty:
   `chrome/app/chrome_crash_reporter_client.cc:131-133`.
5. **Helium patch.** `crash-reporter-privacy-tweaks.patch` inserts
   `if (!DisableCrashpadUploadsAtStartup(*database_path)) return false;`
   before `StartHandler`. In the prepared tree that is
   `components/crash/core/app/crashpad_linux.cc:174-176`; the definition is
   `components/crash/core/app/crashpad.cc:194-198`.
6. `DisableCrashpadUploadsAtStartup("")` calls
   `CrashReportDatabase::Initialize("")`; Crashpad's
   `LoggingCreateDirectory` runs `mkdir("")` and logs at
   `third_party/crashpad/crashpad/util/file/filesystem_posix.cc:63`. It returns
   `nullptr`, so the helper returns false and
   `PlatformCrashpadInitialization` returns false at
   `components/crash/core/app/crashpad_linux.cc:175`.
7. `InitializeCrashpad` returns false
   (`components/crash/core/app/crashpad.cc:200-204`), but
   `ChromeMainDelegate::PreSandboxStartup` ignores the result and still calls
   `crash_reporter::SetFirstChanceExceptionHandler(...)`:
   `chrome/app/chrome_main_delegate.cc:1700-1702` (also the zygote path,
   `:1854-1856`).
8. `crash_reporter::SetFirstChanceExceptionHandler`
   (`components/crash/core/app/crashpad_linux.cc:113-118`) calls
   `CrashpadClient::SetFirstChanceExceptionHandler`
   (`crashpad_client_linux.cc:767-770`). `SignalHandler::Get()` returns the
   static `handler_` (`:148`, defined `:283`), which is assigned only in
   `Install()` (`:192`). `Install()` runs only from
   `RequestCrashDumpHandler::Initialize` (`:390`), reached from
   `CrashpadClient::StartHandler` (`:500-502`), which step 5 skipped.
   The `DCHECK` at `:769` is compiled out, so `:770` stores through null and
   faults at `0x2620`.

## Helium's patch versus upstream Chromium

`DisableCrashpadUploadsAtStartup` is not upstream. It is added by Helium's
`config/patches/.../core/crash-reporter-privacy-tweaks.patch`:

- definition hunk: `@@ -192,6 +191,12 @@` in `components/crash/core/app/crashpad.cc`;
- call-site hunk: `@@ -171,6 +171,10 @@` in `components/crash/core/app/crashpad_linux.cc`.

Without that call-site hunk, `PlatformCrashpadInitialization` goes from
`GetCrashDumpLocation`/`GetCrashMetricsLocation`
(`crashpad_linux.cc:171-172`) straight to
`CHECK(client.StartHandler(...))` (`:270-272`). When the database path is
usable, `StartHandler` runs `Install()` and sets `handler_`
(`crashpad_client_linux.cc:192`). With the empty path, the handler rejects
`--database`, `StartHandler` fails, and the `CHECK` aborts; that is the
behavior measured below.

This was tested, not assumed. Forcing
`crash_reporter::DisableCrashpadUploadsAtStartup` to return true under gdb
(so the code follows the upstream path) does **not** start the browser:

```
helium_crashpad_handler: --database is required
Try 'helium_crashpad_handler --help' for more information.
[...socket.cc:120] recvmsg: Connection reset by peer (104)
Program received signal SIGTRAP ...
0x... PlatformCrashpadInitialization(...) + 3384
=> ...: brk #0x0
```

So changing the gate alone only converts the null-write SEGV into the
`CHECK(client.StartHandler(...))` failure. The null guard (below) is the fix
that lets startup continue.

## musl versus glibc

There is **no libc difference in this path**. It is permission/existence and an
unchecked null, all libc-neutral:

- `base::GetHomeDir()` is `getenv("HOME")` (`file_util_posix.cc:1089`); identical
  under glibc.
- `mkdir("")` is a kernel result, not a libc decision: musl 1.2.6
  `src/stat/mkdir.c:5-11` is a direct `syscall(SYS_mkdir, ...)` (or
  `SYS_mkdirat`), and `src/stat/mkdirat.c:4-7` directly issues `SYS_mkdirat`.
  glibc's `mkdir` issues `mkdirat` too, so both see the kernel's `ENOENT` for
  an empty path and `EACCES` for `/nonexistent`.
- The null dereference and the Helium early return are ordinary C++ and do not
  depend on libc.

A real, observed musl-vs-glibc difference exists nearby but is unrelated:
Crashpad logs `sched_getscheduler: Function not implemented (38)`
(`crashpad/snapshot/linux/process_reader_linux.cc:95`) under musl. It is a
warning and does not cause the fault.

Therefore: Helium built against glibc would take the same SEGV. Upstream
Chromium is different only because it lacks Helium's early return, and would
reach the `CHECK`/`brk` above (on either libc) rather than this null write.

## Proposed fix

Make the first/last-chance setters tolerate a missing Crashpad signal handler,
which is exactly the state `InitializeCrashpad` failure leaves. Intended file:
`config/patches/helium/helium-crashpad-null-signal-handler.patch` (added to
`local_patches` in `config/helium.patch-disposition.json`).

```diff
--- a/third_party/crashpad/crashpad/client/crashpad_client_linux.cc
+++ b/third_party/crashpad/crashpad/client/crashpad_client_linux.cc
@@ -766,13 +766,17 @@
 // static
 void CrashpadClient::SetFirstChanceExceptionHandler(
     FirstChanceHandler handler) {
-  DCHECK(SignalHandler::Get());
+  if (!SignalHandler::Get()) {
+    return;
+  }
   SignalHandler::Get()->SetFirstChanceHandler(handler);
 }
 
 // static
 void CrashpadClient::SetLastChanceExceptionHandler(LastChanceHandler handler) {
-  DCHECK(SignalHandler::Get());
+  if (!SignalHandler::Get()) {
+    return;
+  }
   SignalHandler::Get()->SetLastChanceExceptionHandler(handler);
 }
```

Why it is correct:

- The condition it handles is real and documented by Crashpad itself:
  `SignalHandler::Get()` "May be `nullptr` if no handler has been installed"
  (`crashpad_client_linux.cc:146-148`), and `DumpWithoutCrash` already uses the
  same null check and logs "Crashpad isn't enabled" (`:739-742`). A failed
  `StartHandler` means Crashpad is not enabled; there is no handler slot to
  populate.
- It is local to Crashpad and does not change behavior when Crashpad
  initializes: the handler is non-null and the assignment is unchanged.
- It covers both callers (`chrome_main_delegate.cc:1701` and `:1855`), both of
  which ignore `InitializeCrashpad`'s return value, and any other process type
  that fails Crashpad initialization.
- It retains Helium's privacy gate: the gate still prevents `StartHandler`, and
  now the absence of Crashpad is handled instead of being dereferenced.
- Including the sibling last-chance setter is the same latent bug and the same
  one-line guard.

Patch mechanics verified in scratch (not the source tree):

```
$ patch --batch --dry-run --fuzz=0 --ignore-whitespace -p1 -i helium-crashpad-null-signal-handler.patch
patching file 'third_party/crashpad/crashpad/client/crashpad_client_linux.cc'
DRYRUN_EXIT=0
$ patch --batch --fuzz=0 --ignore-whitespace -p1 -i helium-crashpad-null-signal-handler.patch
APPLY_EXIT=0
```

No existing applied patch touches `crashpad_client_linux.cc`, so the line
numbers are stable when this patch is appended after the current local patches.

Behavior simulated without rebuilding (the task forbids rebuilding Helium): at
the fault, advance `$pc` past the faulting `str` and continue. That is exactly
what the guard does (skip the null write, then run
`crash_keys::SetCrashKeysFromCommandLine`):

```
=== SKIP STORE ===
...
<html><head></head><body></body></html>
[Inferior 1 (process 15) exited normally]
```

the main process serves `about:blank` and exits normally. gdb's batch wrapper
then returns 1 because the script's post-exit `info symbol $pc` has no
registers; that is the script's exit status, not the browser's. (The
`Crashing due to FD ownership violation` lines in that run are gdb fd-inheritance
artifacts in the network service, not the fault. An earlier variant that let gdb
`pass` SIGSEGV instead showed a second SIGSEGV delivered to the program; with
`nopass` the main process exited normally as shown.)

## Not verified

- Not run in a SmolVM guest. The container is `linux/arm64` on the same host;
  the expected guest behavior is identical (same uid-1000 permission check and
  the same Helium binary), but it was not observed in a guest.
- No rebuilt Helium: the task forbids it. The fix is validated by patch
  dry-run/apply and by the gdb "skip the null store" simulation, not by a
  compiled build. A rebuilt archive is required to confirm end to end.
- No upstream glibc Chromium was executed. The upstream comparison is
  source-level plus the in-binary forced-bypass experiment (which exercises the
  upstream `StartHandler` path); the exact upstream outcome on a glibc host with
  yama absent/present was not measured.
- The `helium_crashpad_handler` credentials handshake with an empty
  `--database` was observed only in this container (`yama/ptrace_scope`
  present); the exact `CHECK` behavior on a host without yama is inferred from
  source, not measured.
