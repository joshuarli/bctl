# Chromium process and sandbox contract (Helium 0.20.3.1)

What the patched Helium 0.20.3.1 browser, zygote and renderer need from the Linux kernel for their process model and sandbox. Kernel slices implement what is listed here and nothing else.

## Evidence

- Tree: Chromium 156.0.8078.79 plus Helium core `2f81b9c`, helium-linux `83027a1` and chromium-build's patches, as prepared by `chromium-build --product helium --profile helium-wayland-release prepare` (`image`, `fetch`, `prepare` only). Volume `hydrogen-helium-0.20.3.1-src-amd64`, source root `/work/src`.
- Citations are `path:line` relative to that source root. A read-only copy of the subtrees used lives at `.work/src/helium/` with identical relative paths (disposable, rebuild with the volume).
- `strace-full.txt:N` is line N of `.work/tmp/h/strace-full.txt` (`strace -f` of the shipped archive, sandbox on, Alpine edge x86_64 musl host). `ffall/t.PID:N` is `.work/tmp/h/ffall/` (`strace -ff -e trace=all`, same archive). `status.txt` and `status2.txt` are `/proc/<pid>/status` and namespace-link captures of the same runs. Rows backed only by a trace say `observed-only` and quote the line.
- Archive: `helium-0.20.3.1-linux-x86_64-musl.tar.xz`, sha256 `6853af22b60345c92e0cbe69181aebd8af3fa79842c7f0135d20c9656c5918c6` (verified).
- The traced package is a Wayland-only Ozone build (`ozone_platform_headless=False`); natively it ran against a headless `weston`, and `--headless=new` aborted with `Invalid ozone platform: headless`. Hydrogen now runs `helium-0.20.3.1-r5-linux-x86_64-musl.tar.xz` (sha256 `f4b06e80a097587e508982a3c73f043dbd80e3660aab6d47f72648b6ffc36a05`), the same tree built with `ozone_platform_headless = true` (chromium-build `1761251`), with `--headless=new` and no compositor.

## Decisions and findings against plan.md

| Item | Finding |
| --- | --- |
| Layer-1 sandbox | **Namespace sandbox (user namespace + PID namespace + net namespace).** The browser takes it whenever `Credentials::CanCreateProcessInNewUserNS()` succeeds (`content/browser/zygote_host/zygote_host_impl_linux.cc:115-118`). plan.md's decision holds. |
| Setuid fallback | The setuid helper is only tried when the user-namespace probe fails (`zygote_host_impl_linux.cc:119-122`). The package ships `chrome_sandbox` with mode 0755, not setuid, and the browser looks for `chrome-sandbox` (hyphen) next to the executable (`sandbox/linux/suid/client/setuid_sandbox_host.cc:138`), so the fallback cannot succeed in this package. With no user namespaces the browser dies at startup: `LOG(FATAL) << "No usable sandbox! ..."` (`zygote_host_impl_linux.cc:129-142`). |
| `--headless=new` | Supported by the r5 archive; G5 uses it. The Wayland-client rows below describe the traced Wayland build and no longer apply to G5. |
| TSYNC | **Not used by the renderer.** `SECCOMP_FILTER_FLAG_TSYNC` is only probed with a NULL program (`sandbox/linux/seccomp-bpf/sandbox_bpf.cc:100-102,154`; the probe must return `EFAULT`). Actually installed with TSYNC only by the GPU process (`content/gpu/gpu_main.cc:528`, `sandbox/policy/linux/sandbox_linux.cc:325-328`; `ffall/t.19164:3274`). The renderer installs with `SECCOMP_FILTER_FLAG_SPEC_ALLOW` (`sandbox_bpf.cc:262-263,275`). Linux `seccomp(2)` must still return `EFAULT` for `(SET_MODE_FILTER, TSYNC, NULL)` (a kernel without TSYNC support must return `EINVAL`, which makes `SupportsSeccompSandbox(MULTI_THREADED)` false). |
| USER_NOTIF, TRACE | **Not used.** `UserNotify()` and `Trace()` exist in the DSL (`sandbox/linux/bpf_dsl/bpf_dsl.cc:264-279`) but no shipped policy calls them (`rg` over `sandbox/`, `content/`). `SECCOMP_RET_KILL` is never emitted: the panic function is replaced by a TRAP (`sandbox_bpf.cc:111-118,211`). |
| `SOCK_SEQPACKET` | **Used**, contradicting a stream-only assumption: zygote control socket, fork PID oracle, sandbox IPC socket, GPU broker, crashpad credential socket (rows below). Mojo channels are `SOCK_STREAM`. |
| ptrace | No `ptrace` call anywhere in a full run (`strace -ff -e trace=all`). Only the out-of-process crashpad handler ptraces, and only after a crash (`third_party/crashpad/crashpad/util/linux/scoped_ptrace_attach.cc:26`). The renderer filter turns `ptrace` into a crash trap (`IsDebug` is watched, `baseline_policy.cc:403-410`) and `prctl(PR_SET_PTRACER)` into `EPERM` (`syscall_parameters_restrictions.cc:234`). `ptrace` can stay `ENOSYS`; `PR_SET_PTRACER` must succeed in the browser (`strace-full.txt:202,217`). |
| `SO_PEERCRED` | Not used by the process model. Defined in `mojo/public/cpp/platform/socket_utils_posix.cc:41` for named-channel peer authorization (`:55`, not on the default launch path). The only call in a run is the browser asking the Wayland compositor's credentials: `ui/ozone/platform/wayland/common/wayland_util.cc:427`, `strace-full.txt:1316`. |
| Wayland client (browser only) | `connect(AF_UNIX, $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY)` (`ffall/t.19115:4235`, observed-only), `sendmsg` with `SCM_RIGHTS`, `memfd_create("wayland-cursor", MFD_CLOEXEC\|MFD_ALLOW_SEALING)` (`strace-full.txt:1514`), `mmap(MAP_SHARED)` of it. Renderers never talk to the compositor. |
| `PR_SET_DUMPABLE` | Not called on the namespace path. `SandboxDebugHandling::SetDumpableStatusAndHandlers` (`sandbox/policy/linux/sandbox_debug_handling_linux.cc:67-81`) is only reached from `EnterSuidSandbox` (`content/zygote/zygote_main_linux.cc:119`). Allowed by the renderer filter (`syscall_parameters_restrictions.cc:184`) but unused. |
| uid/gid maps | Written by Chromium, not by the kernel: identity single-id maps, `setgroups` first. Two user namespaces are entered per zygote (launch, then `EngageNamespaceSandbox`). |
| Probes that must not crash | `memfd_create("", ~0)` and `eventfd2(0, ~0)` must fail with `EINVAL` (`ENOSYS`/`EPERM` also accepted), anything else is a `PCHECK` crash (`mojo/core/channel_linux.cc:187-194,943-948`). |

## 1. Process model

| Role | Created by | How | Cite |
| --- | --- | --- | --- |
| Browser | shell/`helium-wrapper` `execve`, then `execve helium` | n/a | `strace-full.txt:1,12` (observed-only, wrapper is a `/bin/sh` script) |
| Unsandboxed zygote (`--type=zygote --no-zygote-sandbox`) | browser | `fork()` + `execvp(FILE_EXE absolute path)`; NNP set before exec | `content/common/zygote/zygote_communication_linux.cc:253-259`, `content/browser/zygote_host/zygote_host_impl_linux.cc:193`, `base/process/launch_posix.cc:354,501-531`; `strace-full.txt:385,399-400` |
| Sandboxed zygote (`--type=zygote`) | browser | `clone(CLONE_NEWUSER\|CLONE_NEWPID\|CLONE_NEWNET\|SIGCHLD)` + pre-exec maps + `execvp(FILE_EXE absolute path)` | `sandbox/linux/services/namespace_sandbox.cc:142,171-214`, `base/process/launch_posix.cc:334-350`; `strace-full.txt:398,431-433` |
| Renderer, sandboxed utility (storage, audio, ...) | sandboxed zygote | `clone(CLONE_NEWPID\|SIGCHLD)` from the zygote (no exec); child is PID 1 of a fresh PID namespace, drops all caps | `content/zygote/zygote_linux.cc:429-432`, `namespace_sandbox.cc:281-302`; `strace-full.txt:3007,17060` |
| GPU | unsandboxed zygote | `fork()` (no exec); installs its own seccomp (TSYNC) later | `content/browser/gpu/gpu_process_host.cc:466-473`; `strace-full.txt:2395` |
| Network service (`sandbox-type=none`) | browser | `fork()` + `execve("/proc/self/exe")` | `content/browser/service_host/utility_sandbox_delegate.cc:141-143`, `content/browser/child_process_host_impl.cc:75-80`; `strace-full.txt:2574` |
| crashpad handler | browser | `fork()` + `execve(helium_crashpad_handler)`; `setsid`; two handlers observed | `third_party/crashpad/crashpad/client/crashpad_client_linux.cc:306-331`; `strace-full.txt:155,182` |
| Renderer launch default | any `SandboxedProcessLauncherDelegate` | generic (sandboxed) zygote unless overridden | `content/browser/sandboxed_process_launcher_delegate.cc:83-87`, `content/browser/child_process_launcher_helper_linux.cc:88-96,187-191` |

Startup order: `SandboxHostLinux::Init`, `ZygoteHostImpl::Init`, `CreateUnsandboxedZygote`, `CreateGenericZygote` before any thread is created (`content/app/content_main_runner_impl.cc:353-376`). `--no-zygote` is rejected unless `--no-sandbox` is also given (`:360-364`). Helium's launch never passes `--no-sandbox`, `--single-process` or `--no-zygote`.

Fork/exec mechanics in the browser child (`base/process/launch_posix.cc`): fds remapped with `dup2`-style shuffle and `CloseSuperfluousFds` by listing `/proc/self/fd` (`:206,466-470`), `PR_SET_NO_NEW_PRIVS` unless `allow_new_privs` (`:501-510`), `PR_SET_PDEATHSIG` only when requested (`:512`, not observed), `execvp` (`:531`). `ForkWithFlags` is `clone(2)` through libc with a throwaway stack and `longjmp` back; `CLONE_VM`, `CLONE_SETTLS`, `CLONE_THREAD`, `CLONE_SIGHAND` are fatal there (`:786-808`, `sandbox/linux/services/syscall_wrappers.cc:43-56`).

Thread creation under musl: `clone(CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_THREAD|CLONE_SYSVSEM|CLONE_SETTLS|CLONE_PARENT_SETTID|CLONE_CHILD_CLEARTID|CLONE_DETACHED)` (`0x400000`); exactly this mask is allowed in sandboxed processes (`syscall_parameters_restrictions.cc:151-156`; `strace-full.txt:194`).

`brk` serializes its complete transition through the shared address space's vmap lock.
The `shared_brk` regression exercises the same shared-VM invariant with separate `CLONE_VM` processes.

## 2. Layer-1 sandbox: selection and sequence

Selection (browser, `content/browser/zygote_host/zygote_host_impl_linux.cc`):

1. `--no-sandbox` returns early (`:89-91`). Running as uid 0 without it exits (`:100-107`).
2. If not `--disable-namespace-sandbox` and `CanCreateProcessInNewUserNS()` then namespace sandbox (`:115-118`).
3. Else if not `--disable-setuid-sandbox` and a `chrome-sandbox` file exists beside the executable then setuid sandbox (`:119-128`).
4. Else `LOG(FATAL) "No usable sandbox!"` (`:129-142`).

Namespace sequence (every step, in order):

| # | Process | Operation | Cite |
| - | --- | --- | --- |
| 1 | browser | Probe: `access("/proc/self/ns/user")` (`PathExists`) | `sandbox/linux/services/namespace_utils.cc:57` |
| 2 | browser | Probe child: `clone(CLONE_NEWUSER\|SIGCHLD)`, errno must be one of `EPERM EUSERS EINVAL ENOSPC` on failure | `sandbox/linux/services/credentials.cc:267-285,125-132`; `strace-full.txt:373` |
| 3 | probe child | write `deny` to `/proc/self/setgroups` (only if the file exists), then `gid_map`, then `uid_map` (`"<id> <id> 1\n"`, one `write` each) | `credentials.cc:163-177`, `namespace_utils.cc:36-49,88-105`; `strace-targeted.txt:13-16` (observed-only for the open order) |
| 4 | probe child | `capset(v3, all zero)`, `capget`, then `unshare(CLONE_NEWUSER)` must succeed (nested namespace with no caps) | `credentials.cc:296-302`; `strace-full.txt:376-378` |
| 5 | browser | `socketpair(AF_UNIX, SOCK_SEQPACKET\|SOCK_CLOEXEC)`, `setsockopt(SO_PASSCRED)` on the browser end | `zygote_host_impl_linux.cc:170-171`; `strace-full.txt:383-384` |
| 6 | browser | `clone(CLONE_NEWUSER\|CLONE_NEWPID\|CLONE_NEWNET\|SIGCHLD)` via `base::LaunchProcess`; `CLONE_NEWNET`/`CLONE_NEWPID` only if `/proc/self/ns/{net,pid}` exist; `SBX_USER_NS`, `SBX_PID_NS`, `SBX_NET_NS` set in the child environment | `namespace_sandbox.cc:171-211`, `namespace_utils.cc:53-84`; `strace-full.txt:398` |
| 7 | zygote child, pre-exec | fd shuffle, `PR_SET_NO_NEW_PRIVS`, then `PreExecDelegate`: `setgroups`=`deny`, `uid_map`, `gid_map` (identity, 1 id), then `execvp` | `namespace_sandbox.cc:44-63`, `launch_posix.cc:501-531`; `strace-full.txt:431-433`, `strace-targeted.txt:22-24` (observed-only for open order) |
| 8 | zygote (PID 1 of new PID ns) | `sendmsg(fd 3, "ZYGOTE_BOOT\0")`; browser reads it with `SO_PASSCRED` and requires the translated sender pid `> 1` | `content/zygote/zygote_main_linux.cc:189-199`, `zygote_host_impl_linux.cc:223-241`; `strace-full.txt:814-815` |
| 9 | zygote | Probes: `prctl(PR_SET_SECCOMP, MODE_FILTER, NULL)` must be `EFAULT`; `seccomp(SET_MODE_FILTER, TSYNC, NULL)` must be `EFAULT` | `sandbox_linux.cc:204-216`, `sandbox_bpf.cc:44-52,100-102`; `strace-full.txt:812-813` |
| 10 | zygote | `open("/proc", O_DIRECTORY\|O_RDONLY\|O_CLOEXEC)` kept as `proc_fd_` for the sandbox lifetime | `sandbox_linux.cc:204-205` |
| 11 | zygote | `MoveToNewUserNS`: `getresuid`/`getresgid` all equal, `unshare(CLONE_NEWUSER)` (second, nested user namespace), then step 3 map writes (order here: `setgroups`, `gid_map`, `uid_map`) | `sandbox_linux.cc:636-640`, `credentials.cc:320-342`; `strace-full.txt:819`, `strace-targeted.txt:26-28` |
| 12 | zygote | `ChrootToSafeEmptyDir`: `clone(CLONE_FS\|SIGCHLD)` helper on a stack buffer; helper does `chroot("/proc/self/fdinfo/")`, `chdir("/")`, `_exit(0)`; zygote `waitpid`s and requires exit 0 | `credentials.cc:44-62,74-121`; `strace-full.txt:820-828` |
| 13 | zygote | Verify: `/proc` is not a directory any more; no open directory among `/proc/self/fd` (uses `fstatat` on each entry and `S_ISDIR`) | `credentials.cc:344-356`, `sandbox/linux/services/proc_util.cc:48,78,102,116` |
| 14 | zygote | `capset(v3)` keeping only `CAP_SYS_ADMIN` (effective and permitted) | `sandbox_linux.cc:646-653`, `credentials.cc:201-218`; `strace-full.txt:833` |
| 15 | zygote | Init reaper: `socketpair(AF_UNIX, SOCK_STREAM)`, `fork()`; parent (PID 1) installs a no-op `SIGCHLD` handler, drops all caps (`capset` zero), sends `C`, loops `waitid(P_ALL, WEXITED)` and exits with the child's code; child (the real zygote) keeps `CAP_SYS_ADMIN` | `sandbox/linux/services/init_process_reaper.cc:27-99`, `zygote_main_linux.cc:127-135`; `strace-full.txt:834-843` |
| 16 | zygote | `sendmsg(fd 3, "ZYGOTE_OK\0")` from the real zygote; browser requires its pid `> 1` and reaps the PID-1 boot process if the pids differ | `content/zygote/zygote_linux.cc:127-138`, `zygote_host_impl_linux.cc:246-262`; `strace-full.txt:844-845` |
| 17 | zygote, per child | `clone(CLONE_NEWPID\|SIGCHLD)`; child asserts `getpid()==1`, `capset` zero, patches musl's cached tid (writes `pthread_self()->tid`), installs termination handlers for `SIGINT SIGTERM SIGHUP SIGQUIT SIGABRT SIGPIPE SIGUSR1 SIGUSR2` | `namespace_sandbox.cc:281-302,305-344`, `zygote_linux.cc:424-452`, `sandbox/linux/services/credentials.cc` (`ForkAndDropCapabilitiesInChild:358-367` is the non-PID-ns variant); `strace-full.txt:3007,17290` |
| 18 | zygote child | `sendmsg(pid_oracle, "CHILD_PING\0")`; browser reads the translated sender pid (`SCM_CREDENTIALS`) and sends it back as `kZygoteCommandForkRealPID`; the child reads it from a pipe | `zygote_linux.cc:453-473,488-520`, `content/common/zygote/zygote_communication_linux.cc:162-182`; `strace-full.txt:2398-2399,3180` |

Resulting observed state (`status.txt`, `status2.txt`):

| Process | `NoNewPrivs` | `Seccomp` | `CapEff` | namespaces vs browser | root |
| --- | --- | --- | --- | --- | --- |
| browser | 0 | 0 | 0 | host | `/` |
| unsandboxed zygote | 1 | 0 | 0 | host | `/` |
| PID-1 boot process of sandboxed zygote | 1 | 0 | 0 | new user, pid, net | `/proc/<helper>/fdinfo` |
| real sandboxed zygote | 1 | 0 | `0x200000` (`CAP_SYS_ADMIN`) | same | same |
| renderer | 1 | 2 (1 filter) | 0 | own PID ns (`NSpid: <host> <zygote-ns> 1`), zygote's user and net ns, host mnt/ipc/uts | `/proc/<helper>/fdinfo` |
| GPU | 1 | 2 | 0 | host | `/` |

The mount namespace is never changed. The new network namespace has no interfaces other than what the kernel creates; the renderers share the zygote's netns.

## 3. Renderer seccomp-BPF

### Installation

| Step | Operation | Cite |
| --- | --- | --- |
| a | `openat(proc_fd, ".", O_RDONLY\|O_DIRECTORY\|O_CLOEXEC)` and `openat(proc_fd, "self/fd/", ...)` | `sandbox_linux.cc:105-122`, `proc_util.cc:48`; `strace-full.txt:17572-17580` |
| b | `fstatat(proc_fd, "self/task/")` must report `st_nlink == 3` (single thread) else retry then crash | `sandbox/linux/services/thread_helpers.cc:40-48`, `sandbox_linux.cc:285-295,363-408` |
| c | `getrlimit`/`setrlimit(RLIMIT_DATA)` (musl issues `prlimit64(0, RLIMIT_DATA, ...)`); bounds brk and private writable non-stack mappings; foreign-pid operations resolve the namespace-visible target and enforce credentials; unmodeled resources fail with EINVAL. Limit 8, 16 or 32 GiB by physical RAM, doubled by `kHigherRendererMemoryLimit` (32 GiB observed) | `sandbox_linux.cc:447-456,476-538`, `sandbox/linux/services/resource_limits.cc:27-46`; `ffall/t.19208:348-351` |
| d | `rt_sigaction(SIGSYS, {SA_SIGINFO\|SA_NODEFER\|SA_RESTORER})`, `rt_sigprocmask(SIG_UNBLOCK, {SIGSYS})` | `sandbox/linux/seccomp-bpf/trap.cc:88-115`; `ffall/t.19208:360` |
| e | Probes: `prctl(PR_SET_SECCOMP, MODE_FILTER, NULL)` (twice), `seccomp(SET_MODE_FILTER, TSYNC, NULL)`, `seccomp(SET_MODE_FILTER, SPEC_ALLOW, NULL)`, all expected `EFAULT` | `sandbox_bpf.cc:44-52,81-102,154,262`; `strace-full.txt:17647-17667,17786` |
| f | `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` (after the zygote already set it, still called) | `sandbox_bpf.cc:245-247`; `strace-full.txt:17780` |
| g | `prctl(PR_GET_SPECULATION_CTRL, PR_SPEC_INDIRECT_BRANCH)`; if `PR_SPEC_PRCTL` set and not forced, `prctl(PR_SET_SPECULATION_CTRL, PR_SPEC_INDIRECT_BRANCH, PR_SPEC_FORCE_DISABLE)`; failures are non-fatal | `sandbox_bpf.cc:282-305`; `strace-full.txt:17792-17802` |
| h | `seccomp(SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_SPEC_ALLOW, &prog)`; fallback `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog)` if `SPEC_ALLOW` unsupported. 682 instructions observed in this build. | `sandbox_bpf.cc:253-279`; `strace-full.txt:17808` (observed-only for the length) |
| i | Sanity: `fchmod(-1, 07777)` must return `EPERM` | `content/renderer/renderer_main_platform_delegate_linux.cc:60-68`, `sandbox_seccomp_bpf_linux.cc:264-281`; `ffall/t.19208:373` |
| j | `close(proc_fd_)` | `sandbox_linux.cc:595-601` |

Entry point: `RendererMainPlatformDelegate::EnableSandbox` (`renderer_main_platform_delegate_linux.cc:32-41`) calls `SandboxLinux::InitializeSandbox(kRenderer, ...)` with default options (`allow_threads_during_sandbox_init=false`, `check_for_open_directories=true`: `sandbox_linux.h:99-114`). `InitializeSandbox` asserts no open directory (`sandbox_linux.cc:427`). Seccomp level is `SINGLE_THREADED` for the renderer (`:325-328`).

### Filter shape

| Property | Value | Cite |
| --- | --- | --- |
| Program prologue | `LD seccomp_data.arch; JEQ AUDIT_ARCH_X86_64` else the panic function (a TRAP into `SANDBOX_DIE`, not a KILL) | `sandbox/linux/bpf_dsl/policy_compiler.cc:128-148`, `sandbox_bpf.cc:111-118,211`, `seccomp_macros.h:90` |
| x32 check | `LD seccomp_data.nr; JSET 0x40000000` then panic ("Illegal mixing of system call ABIs") | `policy_compiler.cc:190-212` |
| Syscall range | numbers `0..1023` evaluated individually; anything else gets `Policy::InvalidSyscall()` = `ERRNO(ENOSYS)` | `linux_syscall_ranges.h:12-14`, `sandbox/linux/bpf_dsl/policy.cc:14-16` |
| cBPF opcodes emitted | `LD\|W\|ABS`, `JMP\|JEQ\|K`, `JMP\|JGE\|K`, `JMP\|JSET\|K`, `ALU\|AND\|K`, `RET\|K`; absolute loads of `nr`, `arch`, and the high/low 32-bit halves of `args[0..5]` | `policy_compiler.cc:144-448`, `seccomp_macros.h:41-50` |
| Actions used | `ALLOW`, `ERRNO(EPERM)`, `ERRNO(ENOSYS)`, `TRAP` (ids `1..N` in the low 16 bits). Never `KILL`, `TRACE`, `USER_NOTIF`, `LOG`. | `bpf_dsl.cc:251-262`, `trap.cc:285-310` |
| Pointer arguments | never inspected (no `copy_from_user`-dependent logic); `clone3` is `ENOSYS` because its flags are behind a pointer | `baseline_policy.cc:196-200` |
| TSYNC | not used for install; probed only | table above |

### Policy (renderer, x86-64, effective order)

`RendererProcessPolicy::EvaluateSyscall` first (`sandbox/policy/linux/bpf_renderer_policy_linux.cc:65-143`), falling through to `BaselinePolicy` (`baseline_policy.cc:108-411`).

| Action | Syscalls | Cite |
| --- | --- | --- |
| ALLOW | `brk mlock munlock munmap mseal`; `sched_yield pause nanosleep`; `epoll_create epoll_create1 epoll_ctl epoll_wait epoll_pwait`; `eventfd eventfd2`; `fstat ftruncate`; `lseek poll ppoll pselect6 select read readv pread64 recvfrom recvmsg recvmmsg write writev`; `gettimeofday time`; `exit exit_group wait4 waitid`; `rt_sigaction rt_sigprocmask rt_sigreturn rt_sigtimedwait sigaltstack`; `capget getegid geteuid getgid getgroups getpid getppid getresgid getresuid getsid gettid getuid`; `restart_syscall`; `close dup dup2 dup3 shutdown`; `uname`; `rseq`; `pkey_free`; `mincore` | `syscall_sets.cc:545,708,478,881,212,598,28,432,353,277,419,396`; `baseline_policy.cc:135,160,184,301` |
| ALLOW (renderer only) | `fdatasync fsync ftruncate getrlimit setrlimit mremap pwrite64 sched_get_priority_max sched_get_priority_min sysinfo times uname` | `bpf_renderer_policy_linux.cc:84-122` |
| ALLOW, argument-checked | `sendmsg sendto` flags `⊆ {MSG_DONTWAIT, MSG_NOSIGNAL}` (else crash trap); `socketpair` domain `AF_UNIX`; `getsockopt setsockopt` only `SOL_SOCKET/SO_PEEK_OFF`; `pipe`; `pipe2` flags `⊆ {O_CLOEXEC,O_DIRECT,O_NONBLOCK}`; `memfd_create` flags `⊆ {MFD_CLOEXEC,MFD_ALLOW_SEALING,MFD_NOEXEC_SEAL}` or exactly `~0`; `mmap` flags `⊆ {SHARED,PRIVATE,ANONYMOUS,STACK,NORESERVE,FIXED,DENYWRITE,LOCKED,DROPPABLE}` (no `MAP_HUGETLB`, no `MAP_POPULATE`); `mprotect pkey_mprotect` prot `⊆ {R,W,X}`; `madvise` advice `∈ {DONTNEED,WILLNEED,RANDOM,REMOVE,NORMAL,FREE}` (others `EPERM`); `futex` op `∈ {WAIT,WAKE,REQUEUE,CMP_REQUEUE,WAKE_OP,WAIT_BITSET,WAKE_BITSET}` ± `PRIVATE`/`CLOCK_REALTIME` (else crash trap); `fcntl` cmd `∈ {F_GETFL,F_GETFD,F_GET_SEALS,F_SETFD,F_SETLK,F_SETLKW,F_GETLK,F_DUPFD,F_DUPFD_CLOEXEC,F_SETFL,F_ADD_SEALS}` (`F_SETFL` mask `O_ACCMODE\|O_APPEND\|O_NONBLOCK\|O_SYNC\|O_LARGEFILE\|O_CLOEXEC\|O_NOATIME`, `F_ADD_SEALS` mask `SEAL\|GROW\|SHRINK`); `prctl` option `∈ {PR_GET_NAME,PR_SET_NAME,PR_GET_DUMPABLE,PR_SET_DUMPABLE}` or `PR_SET_VMA` with `PR_SET_VMA_ANON_NAME`; `clock_gettime clock_nanosleep clock_getres` clock id `∈ {BOOTTIME,MONOTONIC,MONOTONIC_COARSE,MONOTONIC_RAW,PROCESS_CPUTIME_ID,REALTIME,REALTIME_COARSE,THREAD_CPUTIME_ID}`; `ioctl` request `∈ {TCGETS, FIONREAD, DMA_BUF_IOCTL_SYNC}`; `getrandom` flags `⊆ {GRND_NONBLOCK,GRND_INSECURE}`; `membarrier` cmd `∈ {REGISTER_PRIVATE_EXPEDITED, PRIVATE_EXPEDITED}`, flags 0 (musl patch); `pkey_alloc` flags 0; `prlimit64` pid `0` or self; `getpriority setpriority sched_get* sched_set{param,scheduler,affinity}` target `0` or self (otherwise rewritten by SIGSYS handler); `kill tgkill` target self only; `tkill` only for a renderer that is PID 1 in its own user+PID namespace; `rt_tgsigqueueinfo` tgid self; `setitimer` `ITIMER_REAL` | `baseline_policy.cc:142-411`, `syscall_parameters_restrictions.cc:142-535`, `bpf_renderer_policy_linux.cc:44-51,65-76,136-138`, `sandbox_seccomp_bpf_linux.cc:192-202` |
| ALLOW, clone | `clone` only with `flags ∈ {glibc-pthread mask, glibc-pthread mask \| CLONE_DETACHED}` (the musl thread mask above) | `syscall_parameters_restrictions.cc:151-156,170` |
| ERRNO(EPERM) | `clone` fork-style (`flags & (CLONE_VM\|CLONE_THREAD) == 0`) or `CLONE_VFORK\|CLONE_VM`; `fork vfork`; `sendfile`; `set_robust_list`; every `IsFileSystem` call (`open openat stat lstat newfstatat statx(ENOSYS for STATX_BASIC_STATS) access readlink rename unlink mkdir execve statfs truncate utimensat` etc.); `getcwd chdir fchdir`; `fallocate fchmod fchown getdents getdents64`; `accept accept4 bind connect socket listen`; `capset ioperm iopl setuid setgid setgroups setres[ug]id setre[ug]id setfs[ug]id`; `umask`; all SysV IPC; `seccomp`; `madvise` bad advice; `setitimer` other timers; `prlimit64` other pids; `prctl(PR_SET_PTRACER)`; `rt_tgsigqueueinfo` foreign tgid; `getpriority/setpriority` foreign | `baseline_policy.cc:142-143,220-243,319-366,385-389`, `syscall_sets.cc:104,80,306,499,1000`, `syscall_parameters_restrictions.cc:234` |
| ERRNO(ENOSYS) | `clone3`; `pidfd_open`; `statx` with `mask == STATX_BASIC_STATS`; any number outside `0..1023` or unlisted by `InvalidSyscall` | `baseline_policy.cc:198,205,328-331`, `policy.cc:14-16` |
| TRAP, rewrite handler | `newfstatat` (`fstatat`): with an empty path and flags exactly `AT_EMPTY_PATH` the handler runs `fstat(fd)` and returns its result, otherwise it returns `-EPERM` (the filesystem-denied errno) from the handler; `sched_*` whose pid argument equals the caller's tid is re-issued with pid 0, any other pid crashes | `baseline_policy.cc:319-321`, `syscall_parameters_restrictions.cc:415-418`, `sigsys_handlers.cc:348-380,382-398,436-443` |
| TRAP, crash handler | everything else (`CrashSIGSYS`, `...Clone`, `...Prctl`, `...Ioctl`, `...Kill`, `...Futex`, `...Ptrace`, `...Socket`, `...Sockopt`): handler writes a message to fd 2, sets a crash key, then dereferences an address derived from the syscall arguments (low page) so the process dies of `SIGSEGV`, with `_exit(1)` as backstop | `baseline_policy.cc:403-410`, `sandbox/linux/seccomp-bpf-helpers/sigsys_handlers.cc:142-300` |

Notable deliberate denials that must not be silently succeeded in the guest: `ptrace`, `arch_prctl` after startup, `setns`, `unshare`, `mount`, `chroot`, `kill` of other pids, `socket`, `connect`, `bind`, `open*`.

### SIGSYS trap handler contract (`sandbox/linux/seccomp-bpf/trap.cc`)

| Expectation | Cite |
| --- | --- |
| Delivered with `si_signo = SIGSYS`, `si_code == SYS_SECCOMP`, `si_errno` = the low 16 bits of the `SECCOMP_RET_TRAP` value, in `1 .. trap_array_size` (the registered trap id; `0` or out of range is logged as "Unexpected SIGSYS" and ignored) | `trap.cc:160-169`, `:285-310` |
| `siginfo` carries `si_call_addr`, `si_syscall`, `si_arch` (`_sigsys` union member at offset 16) | `trap.cc:171-182` |
| `si_call_addr == ucontext.gregs[REG_RIP]`, `si_syscall == (int)gregs[REG_RAX]`, `si_arch == AUDIT_ARCH_X86_64`; any mismatch is `RAW_SANDBOX_DIE` | `trap.cc:191-203`, `seccomp_macros.h:90-102` |
| Arguments read from `gregs[REG_RDI, RSI, RDX, R10, R8, R9]` | `trap.cc:242-251` |
| Return value is stored into `gregs[REG_RAX]` and the handler returns normally: the kernel's `rt_sigreturn` must resume after the `syscall` instruction with `RAX` taken from the frame | `trap.cc:261`, `sandbox/linux/seccomp-bpf/syscall.cc` (`PutValueInUcontext`) |
| Handler installed with `SA_SIGINFO\|SA_NODEFER`; `SIGSYS` is unblocked process-wide; `SIGBUS` is blocked inside "unsafe" traps as a marker (unused: no unsafe traps in Helium) | `trap.cc:94-95,109-114,64-75` |
| A previous non-default `SIGSYS` disposition is only logged; `SIGSYS` is reserved | `trap.cc:101-107` |
| Rewrite handlers re-issue `sched_*` and `fstat` from handler context under the same filter, and read the user path pointer of `newfstatat` | `sigsys_handlers.cc:348-398` |
| A crash handler's own `write(2, ...)` must be allowed and a store to a near-zero address must raise `SIGSEGV` | `sigsys_handlers.cc:70-89,142-165` |

## 4. Per-role tables

### Browser

| Kind | Operation | Cite |
| --- | --- | --- |
| process | `fork`, `execve` (listed in section 1); `clone(... NEWUSER\|NEWPID\|NEWNET\|SIGCHLD)`; `clone(CLONE_NEWUSER\|SIGCHLD)` probe | `launch_posix.cc:334-354`, `credentials.cc:279-281` |
| process | `wait4`/`waitpid` (probe child, reaping), `kill(pid, SIGTERM/SIGKILL)` | `credentials.cc:309`, `zygote_linux.cc:86-87`; `ffall/t.19160:1694` (observed-only for SIGTERM at shutdown) |
| prctl | `PR_SET_NO_NEW_PRIVS` (children, pre-exec); `PR_SET_PTRACER` (browser sets crashpad handler as ptracer); `PR_SET_NAME`; `PR_SET_VMA_ANON_NAME` (EINVAL tolerated) | `launch_posix.cc:501`, `crashpad_client_linux.cc:317,384,411,439`, `base/threading/platform_thread_linux.cc:226`; `strace-full.txt:202,217,705-712` |
| socket | `socketpair(AF_UNIX, SOCK_SEQPACKET\|SOCK_CLOEXEC)` zygote control; `socketpair(AF_UNIX, SOCK_SEQPACKET)` per fork request (PID oracle); sandbox-IPC pair `SOCK_SEQPACKET` with `shutdown(SHUT_RD)`/`shutdown(SHUT_WR)`; Mojo `socketpair(AF_UNIX, SOCK_STREAM)` + `fcntl(O_NONBLOCK)` | `zygote_host_impl_linux.cc:170`, `zygote_communication_linux.cc:116`, `content/browser/sandbox_host_linux.cc:30-46`, `mojo/public/cpp/platform/platform_channel.cc:154-158`; `strace-full.txt:144,364,383` |
| sockopt | `setsockopt(SOL_SOCKET, SO_PASSCRED, 1)` on the receiving end of every `SOCK_SEQPACKET` credential pair | `base/posix/unix_domain_socket.cc:44-48` |
| cmsg | send `SCM_RIGHTS` (`CMSG_SPACE(n*4)`, `MSG_NOSIGNAL`); receive `SCM_RIGHTS` and `SCM_CREDENTIALS` with a control buffer for 16 fds + one `ucred`; `MSG_TRUNC` -> `EMSGSIZE`, `MSG_CTRUNC` -> fds dropped | `unix_domain_socket.cc:55-97,107-217`, `kMaxFileDescriptors = 16` at `unix_domain_socket.h:32` |
| fd shuffle | `/proc/self/fd` listing, `dup2`, `fcntl(F_SETFD)` | `launch_posix.cc:206,466` |
| proc | `/proc/self/ns/{user,pid,net,ipc,mnt,uts}` existence; `/proc/self/{setgroups,uid_map,gid_map}`; `/proc/sys/kernel/yama/ptrace_scope` (read; `ENOENT` ok); `/proc/self/exe` | `namespace_utils.cc:57-84`, `sandbox/linux/services/yama.cc:74-80`, `child_process_host_impl.cc:75-80` |
| probes | `landlock_create_ruleset(NULL,0,VERSION)` result only reported; `ENOSYS`/`EOPNOTSUPP` are fine | `sandbox/policy/linux/landlock_util.cc:25-33`; `ffall/t.19115:2675` (observed-only) |
| shm | `memfd_create("shared-memory-region", MFD_CLOEXEC\|MFD_ALLOW_SEALING\|MFD_NOEXEC_SEAL)`, retry without `MFD_NOEXEC_SEAL` on `EINVAL`; `ftruncate`; `F_ADD_SEALS(SHRINK\|GROW\|SEAL)`; writable region also `open("/proc/self/fd/N", O_RDONLY\|O_CLOEXEC)` for the read-only fd; `mmap(MAP_SHARED, PROT_READ[\|PROT_WRITE])` | `base/memory/platform_shared_memory_region_posix.cc:88-148`, `platform_shared_memory_mapper_posix.cc:21`; `strace-full.txt:137` |
| shm fallback | when `memfd_create` fails (`ENOSYS`): unlinked file in the shared-memory temp dir (`/dev/shm`), reopened read-only by path | `platform_shared_memory_region_posix.cc:300-361` |
| poll | `epoll_create1(0)`, `eventfd(0, EFD_NONBLOCK)`, `epoll_ctl(ADD/MOD/DEL)` with `EPOLLIN\|EPOLLOUT\|EPOLLRDHUP\|EPOLLPRI\|EPOLLERR\|EPOLLHUP`, `EPOLLONESHOT`, `epoll_wait`; musl issues `epoll_pwait` | `base/message_loop/message_pump_epoll.cc:142-149,328,385,405,471`; `ffall/` (`epoll_pwait` 12203 calls, observed-only) |
| futex | `FUTEX_WAIT/WAKE` (+`PRIVATE`), `FUTEX_CMP_REQUEUE`, `FUTEX_WAIT_BITSET`/`WAKE_BITSET`; also by musl pthread primitives | `syscall_parameters_restrictions.cc:374-390`; `ffall/` (21009 `futex`, observed-only) |
| threads | musl thread `clone` mask; `set_tid_address`, `sched_setscheduler`/`sched_setaffinity`, `setpriority`, `gettid` | `platform_thread_linux.cc:277-281`, `platform_thread_posix.cc:237-263` |
| misc | `getrandom`, `uname`, `membarrier(REGISTER_PRIVATE_EXPEDITED)`, `inotify_init1`/`add_watch`, `pwritev2` (net service), netlink `socket(AF_NETLINK)` + `bind` (network service) | `baseline_policy.cc:164-177,255`; `ffall/t.19143:37`, `ffall/t.19142:188` (observed-only) |

### Zygote (sandboxed, then forking)

| Kind | Operation | Cite |
| --- | --- | --- |
| sandbox | all of section 2 steps 8-18 | section 2 |
| fork | `clone(CLONE_NEWPID\|SIGCHLD)` per child; `fork()` in the reaper; `clone(CLONE_FS\|SIGCHLD)` helper | `zygote_linux.cc:429-432`, `init_process_reaper.cc:34`, `credentials.cc:97,113` |
| wait | `wait4(pid, WNOHANG)`, `waitpid(pid, 0)` after `kill(pid, SIGKILL)`, `waitid(P_ALL, 0, WEXITED)` in the PID-1 reaper | `zygote_linux.cc:86-87,174-190`, `init_process_reaper.cc:68-69`; `strace-full.txt:843` |
| signal | `sigaction(SIGCHLD, no-op)`; `sigprocmask(SIG_BLOCK, SIGCHLD)`; `ppoll` on fd 3 with `SIGCHLD` unblocked | `zygote_linux.cc:110-160` |
| socket | fd 3 = `SOCK_SEQPACKET` from the browser (`kZygoteSocketPairFd = kBaseDescriptor = 3`); `recvmsg` with `SCM_RIGHTS`; `sendmsg` replies; pid-oracle `sendmsg` | `content/common/zygote/zygote_commands_linux.h:16-36`, `base/posix/global_descriptors.h:59`, `zygote_linux.cc:230-275` |
| cmsg | each fork request carries `1 + n` fds: the pid-oracle socket and the child's fd map (Mojo channel, crash-dump signal fd, sandbox IPC fd, field-trial and histogram shared memory) | `zygote_communication_linux.cc:121-160`, `content/public/common/content_descriptors.h:13-23` |
| proc | `/proc/self/fd/` (`openat(proc_fd, "self/fd/")`), `fstatat(proc_fd, "self/task/")`, `/proc/self/fdinfo/` as chroot target | `proc_util.cc:48,78`, `thread_helpers.cc:40`, `credentials.cc:55` |
| caps | `capset(v3)` three times: keep `CAP_SYS_ADMIN`; drop all in PID-1 boot process; drop all in each forked child | `credentials.cc:201-218`, `namespace_sandbox.cc:292` |
| none | The zygote itself has no seccomp filter (`Seccomp: 0`, `status.txt`) | `zygote_main_linux.cc:162-227` |

### Renderer (after fork from the zygote)

| Kind | Operation | Cite |
| --- | --- | --- |
| entry | fork child returns from `ZygoteMain`, `getpid()==1`, `capset` zero (no-op), `CHILD_PING`, reads real pid from a pipe | section 2 step 17-18 |
| sandbox | section 3 install sequence | section 3 |
| ipc | Mojo `SOCK_STREAM` channel: `send(..., MSG_NOSIGNAL)` (musl: `sendto`), `sendmsg` with `SCM_RIGHTS` (>=1 byte of payload per fd batch, at most `kMaxSendmsgHandles = 128` fds per message), `recvmsg(MSG_DONTWAIT)`, `shutdown(SHUT_RDWR)` | `mojo/public/cpp/platform/socket_utils_posix.cc:70,82-97,106-135`, `socket_utils_posix.h:24`, `mojo/core/channel_posix.cc:257,372-445`; `ffall/t.19208` (`sendto`, `recvmsg`, observed-only) |
| shm | maps received memfds (`MAP_SHARED`); read-only regions arrive as `O_RDONLY` fds; creates its own via `memfd_create` (allowed flags above); Mojo shared-memory upgrade creates `memfd_create("mojo_channel_linux", MFD_CLOEXEC\|MFD_ALLOW_SEALING)`, `ftruncate`, `F_ADD_SEALS(SEAL\|SHRINK\|GROW)`, `fcntl(F_GET_SEALS)` must equal exactly those seals, `eventfd2(0, EFD_CLOEXEC\|EFD_NONBLOCK)` | `mojo/core/channel_linux.cc:85,112-148,162-171,860-900`, `platform_shared_memory_region_posix.cc:88-148` |
| threads | thread `clone` mask, `futex`, `sched_yield`, `nanosleep`, `madvise(DONTNEED)`, `mprotect`, `mremap`, `membarrier(REGISTER_PRIVATE_EXPEDITED)`, `prctl(PR_SET_NAME)`, `prctl(PR_SET_VMA, PR_SET_VMA_ANON_NAME)` | `ffall/t.19208` (all observed-only); `baseline_policy.cc:164-177`, `syscall_parameters_restrictions.cc:179-235` |
| tolerated failures | `madvise(MADV_DODUMP/DONTDUMP)` -> `EPERM`; `setpriority(PRIO_PROCESS,0,-8)` -> `EACCES`; `prlimit64(RLIMIT_NPROC)` -> `EFAULT`; `memfd_create(~0)`/`eventfd2(~0)` -> `EINVAL`; `PR_SET_VMA` -> `EINVAL` | `ffall/t.19208:665,321,465`; `strace-full.txt:705-712` (observed-only) |
| denied | file access, sockets, `ptrace`, `setns`, `execve` (section 3) | section 3 |

### Other child types observed (not in scope, listed for completeness)

Utility sandboxed processes use `UtilityProcessPolicy`; GPU uses `GpuProcessPolicy` and a signal-based syscall broker forked as `--type=broker` (`socketpair(AF_UNIX, SOCK_SEQPACKET)` at `sandbox/linux/syscall_broker/broker_channel.cc:21-24`; `SCM_RIGHTS`/`SCM_CREDENTIALS` replies at `broker_simple_message.cc:95-209`; `fork` at `broker_process.cc:68`). The network service runs unsandboxed here (`--service-sandbox-type=none`).

## 5. Crashpad credential socket

| Operation | Cite |
| --- | --- |
| Pair: `socketpair(AF_UNIX, SOCK_SEQPACKET)` then `setsockopt(SOL_SOCKET, SO_PASSCRED, 1)` on both ends (**must succeed**; this is the call that returns `ENOTSUP` on stock Nanos) | `third_party/crashpad/crashpad/util/linux/socket.cc:29-52`; `strace-full.txt:172-174` |
| Handler start: `fork` + `execve`; the handler also does `getsockopt(SO_PASSCRED)` on its end (observed-only, `strace-full.txt:198`, origin not found in the tree); client then `recvmsg` with `SCM_CREDENTIALS`, `PR_SET_PTRACER` to the handler pid, `pthread_atfork(SetPtracerAtFork)` | `crashpad_client_linux.cc:306-331,366-385,436-440,462-505`; `strace-full.txt:198-202` |
| Message exchange: `sendmsg`/`recvmsg` with `SCM_RIGHTS` (up to `kMaxSendRecvMsgFDs`) and `SCM_CREDENTIALS`, `MSG_NOSIGNAL`; `MSG_TRUNC`/`MSG_CTRUNC` are errors | `socket.cc:55-93,103-180`, `exception_handler_client.cc:71-83,144-153` |
| Crash path only: `fork` of a ptrace broker, `ptrace(PTRACE_ATTACH/DETACH)`, `/proc/<pid>/task` reads (`util/linux/ptrace_client.cc:269`), `sigaltstack`, `rt_tgsigqueueinfo` to re-raise; `kDumpDoneSignal` + `sigtimedwait` in multi-client mode | `exception_handler_client.cc:119-141,163-197`, `util/linux/scoped_ptrace_attach.cc:26`, `client/crashpad_client_linux.cc:518-604`, `util/posix/signals.cc:319-324` |
| Renderers inherit the handler socket as fd `kCrashDumpSignal`; `PR_SET_PTRACER` from a renderer is `EPERM` and tolerated | `components/crash/core/app/crashpad_linux.cc:270-288`, `syscall_parameters_restrictions.cc:234` |
| Runtime note: under musl `sched_getscheduler` returns `ENOSYS` in the handler (`process_reader_linux.cc:95` warning, observed-only); harmless | `run.log`, not a kernel requirement |

## 6. Sandbox-touching patches in the tree

| Patch (chromium-build `config/patches/` unless noted) | Effect on the contract |
| --- | --- |
| `musl-seccomp-clone-detached.patch` | Thread `clone` mask also accepted with `CLONE_DETACHED` (`0x400000`), which musl always passes (`syscall_parameters_restrictions.cc:154-156`) |
| `musl-seccomp-membarrier.patch` | `membarrier(REGISTER_PRIVATE_EXPEDITED\|PRIVATE_EXPEDITED, 0)` allowed, other forms `EPERM` (`baseline_policy.cc:164-177`); the kernel must implement `membarrier` private expedited (musl registers it at first thread creation) |
| `musl-sandbox-cmsg-sign-compare.patch`, `musl-crashpad-cmsg.patch`, `musl-unix-domain-socket-types.patch`, `musl-mojo-cmsg-sign-compare.patch` | Type/warning fixes for `cmsg_len` under musl; no behavioural change; musl's `CMSG_LEN`/`CMSG_NXTHDR` layout is the one the kernel must produce |
| `musl-gpu-sandbox-broker-tsync.patch`, `musl-gpu-sandbox-tsync.patch` | GPU process may initialize its sandbox with multiple threads (`allow_threads_during_sandbox_init`, `gpu_main.cc:528`) so it installs with `SECCOMP_FILTER_FLAG_TSYNC`; the broker `Fork` accepts threads (`broker_process.cc`) |
| `musl-gpu-pwritev2-sandbox.patch` | GPU policy allows `pwritev2` (the net service also issues it) |
| `musl-gpu-sched-affinity-errno.patch` | GPU `sched_setaffinity` returns `EPERM` instead of crashing |
| Alpine `musl-conditional-tkill.patch` | Renderer filter allows `tkill` when `getpid()==1` and both `kUserNS` and `kPIDNS` are set (`sandbox_seccomp_bpf_linux.cc:192-202`, `bpf_renderer_policy_linux.cc:67-68`): musl `pthread_kill` uses `tkill` |
| Alpine `musl-tid-caching.patch` | After `clone(CLONE_NEWPID)` the child overwrites musl's cached `tid` with `gettid()` (`namespace_sandbox.cc:273-298`): the kernel must return the namespace-local tid (1 for the first thread) |
| Alpine `no-sandbox-settls.patch` (`config/helium.patch-disposition.json:34`) | The `CLONE_VM\|CLONE_VFORK\|CLONE_SETTLS` shortcut in `ChrootToSafeEmptyDir` is compiled out (`credentials.cc:99`): the chroot helper is a plain `clone(CLONE_FS\|SIGCHLD)` |
| `headless-no-crashpad-zygote.patch` | Only the headless variant; Helium keeps `InitializeCrashpad` in `ZygoteForked` (`chrome/app/chrome_main_delegate.cc:1692,1700,1836`) |

The Alpine `musl-sandbox.patch` (renderer `sched_*`, `mremap`, `set_tid_address`) is not part of the Helium disposition and not applied: `baseline_policy.cc:250-253` still restricts `sched_*` to self.

## 7. Kernel requirements (what later slices must provide)

Process and namespaces:

1. Unprivileged `clone`/`unshare` of `CLONE_NEWUSER`, nested at least two deep; full capability set inside a new user namespace; `/proc/self/{setgroups,uid_map,gid_map}` with the write order and format above; `CLONE_NEWPID` and `CLONE_NEWNET` from inside it; `/proc/self/ns/{user,pid,net}` present.
2. `getpid()==1`, `gettid()` namespace-local, `waitid(P_ALL, WEXITED)` and `wait4(WNOHANG)` for the reaper; `SIGCHLD`; PID-1 death kills its namespace.
3. `SCM_CREDENTIALS` / `SO_PASSCRED` on `AF_UNIX` `SOCK_SEQPACKET` with the sender pid translated into the receiver's PID namespace (browser must see a pid `> 1`); implicit credentials when the receiver set `SO_PASSCRED`.
4. `chroot("/proc/self/fdinfo/")` from a `CLONE_FS` helper that exits; the root stays; `/proc` absent afterwards; `fstatat(proc_fd, "self/task/")` with `st_nlink == 2 + threads`; `/proc/self/fd` listing where each entry `fstatat`s as directory or not; `capget`/`capset` v3.
5. `prctl`: `PR_SET_NO_NEW_PRIVS`, `PR_SET_PTRACER`, `PR_SET_NAME`, `PR_GET_NAME`, `PR_SET_VMA/PR_SET_VMA_ANON_NAME` (may fail `EINVAL`), `PR_GET_SPECULATION_CTRL`/`PR_SET_SPECULATION_CTRL` (may fail), `PR_SET_PDEATHSIG` (optional), `PR_SET_SECCOMP` and `seccomp(2)` incl. the NULL-program `EFAULT` probes; unknown `prctl` options must return `EINVAL`.
6. Seccomp as in section 3 (ALLOW/ERRNO/TRAP only, `SPEC_ALLOW` flag accepted, TSYNC accepted for the GPU path only, 682-instruction program, `AUDIT_ARCH_X86_64` and x32 rejection, ids in `si_errno`, exact `SIGSYS` frame).
7. `landlock_create_ruleset(NULL,0,VERSION)` may return `ENOSYS`.

IPC and memory:

8. `AF_UNIX` `SOCK_STREAM` and `SOCK_SEQPACKET` `socketpair`; `SCM_RIGHTS` (up to 128 fds per message from Mojo; 16-fd receive buffers in `UnixDomainSocket`); `MSG_NOSIGNAL`, `MSG_DONTWAIT`, `MSG_TRUNC`, `MSG_CTRUNC`; `shutdown(SHUT_RD/WR/RDWR)`; SCM_RIGHTS on a stream socket needs at least one payload byte.
9. `memfd_create` with `MFD_CLOEXEC|MFD_ALLOW_SEALING|MFD_NOEXEC_SEAL` (and `EINVAL` for `MFD_NOEXEC_SEAL` on kernels without it, and for `~0`), `F_ADD_SEALS`/`F_GET_SEALS` (`SHRINK`, `GROW`, `SEAL`; `F_SEAL_WRITE` is not used), `ftruncate`, `open("/proc/self/fd/N", O_RDONLY)` of a memfd yielding a read-only description of the same file, and `mmap(MAP_SHARED, PROT_WRITE)` of that read-only fd failing; `mmap(MAP_SHARED)` of memfds in several processes sees the same pages.
10. `eventfd`/`eventfd2` (`EFD_CLOEXEC|EFD_NONBLOCK`, `EINVAL` on bad flags), `epoll_create1/ctl/pwait`, `futex` private and shared (`WAIT`, `WAKE`, `REQUEUE`, `CMP_REQUEUE`, `WAKE_OP`, `WAIT_BITSET`, `WAKE_BITSET`, `CLOCK_REALTIME`), `membarrier(PRIVATE_EXPEDITED)`, `getrandom`, `uname` (release `>= 4.0` for the Mojo shared-memory upgrade, otherwise it silently falls back), `prlimit64(RLIMIT_DATA)`.
11. `ptrace`, `setns`, `mount` may stay unimplemented: nothing in browser, zygote or renderer calls them.
