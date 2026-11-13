#define SECCOMP_SET_MODE_STRICT 0
#define SECCOMP_SET_MODE_FILTER 1
#define SECCOMP_GET_ACTION_AVAIL 2
#define SECCOMP_GET_NOTIF_SIZES 3
#define SECCOMP_MODE_DISABLED 0
#define SECCOMP_MODE_STRICT 1
#define SECCOMP_MODE_FILTER 2
#define SECCOMP_FILTER_FLAG_TSYNC (1UL << 0)
#define SECCOMP_FILTER_FLAG_SPEC_ALLOW (1UL << 2)
#define SECCOMP_FILTER_FLAG_TSYNC_ESRCH (1UL << 4)
#define SECCOMP_RET_KILL_PROCESS 0x80000000U
#define SECCOMP_RET_KILL_THREAD 0x00000000U
#define SECCOMP_RET_KILL SECCOMP_RET_KILL_THREAD
#define SECCOMP_RET_TRAP 0x00030000U
#define SECCOMP_RET_ERRNO 0x00050000U
#define SECCOMP_RET_USER_NOTIF 0x7fc00000U
#define SECCOMP_RET_TRACE 0x7ff00000U
#define SECCOMP_RET_LOG 0x7ffc0000U
#define SECCOMP_RET_ALLOW 0x7fff0000U
#define SECCOMP_RET_ACTION_FULL 0xffff0000U
#define SECCOMP_RET_ACTION 0x7fff0000U
#define SECCOMP_RET_DATA 0x0000ffffU
#define SYS_SECCOMP_SI_CODE 1
#if defined(__x86_64__)
#define AUDIT_ARCH_NATIVE 0xc000003eU
#elif defined(__aarch64__)
#define AUDIT_ARCH_NATIVE 0xc00000b7U
#elif defined(__riscv)
#define AUDIT_ARCH_NATIVE 0xc00000f3U
#else
#error unsupported seccomp architecture
#endif
#define BPF_MAXINSNS 4096
#define BPF_MEMWORDS 16
struct sock_filter {
    u16 code;
    u8 jt;
    u8 jf;
    u32 k;
};
struct sock_fprog {
    u16 len;
    struct sock_filter *filter;
};
struct seccomp_data {
    int nr;
    u32 arch;
    u64 instruction_pointer;
    u64 args[6];
};
typedef struct seccomp_filter {
    struct refcount refcount;
    struct seccomp_filter *prev;
    u32 path_insns;
    u16 len;
    closure_struct(thunk, free);
    struct sock_filter insns[];
} *seccomp_filter;
boolean seccomp_check(thread t, context_frame f, sysreturn *rv);
sysreturn seccomp_validate(struct sock_filter *insns, u16 len);
u32 seccomp_run(seccomp_filter chain, const struct seccomp_data *sd, u16 *data);
sysreturn seccomp(u32 op, u32 flags, void *uargs);
sysreturn seccomp_prctl(u64 mode, void *prog);
void seccomp_fork(thread child, thread parent);
void seccomp_thread_exit(thread t);
void syscall_dispatch(thread t, syscall_context sc, context_frame f);
void force_signal(thread t, struct siginfo *info);
void seccomp_reserve(seccomp_filter f);
void seccomp_release(seccomp_filter f);
u32 seccomp_filter_count(seccomp_filter f);
