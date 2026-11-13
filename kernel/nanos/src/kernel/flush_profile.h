#ifdef HYDROGEN_FLUSH_PROFILE
struct flush_profile {
    word posts;
    word rendezvous_posts;
    word pages;
    word full;
    word handled;
    word rendezvous_wait_ns;
    word join_wait_ns;
    word user_loads;
    word kernel_loads;
    word faults;
    word fault_lock_wait_ns;
    word fault_paging_ns;
    word wakeup_ipis;
    word steals;
    word pushes;
};
extern struct flush_profile flush_profile;
#define flush_profile_add(field, n) fetch_and_add(&flush_profile.field, (word)(n))
#define flush_profile_now() kern_now(CLOCK_ID_MONOTONIC_RAW)
void flush_profile_start(void);
#else
#define flush_profile_add(field, n) do { (void)(n); } while (0)
#define flush_profile_now() 0
#define flush_profile_start() do {} while (0)
#endif
