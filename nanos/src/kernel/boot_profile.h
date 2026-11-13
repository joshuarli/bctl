#ifdef HYDROGEN_BOOT_PROFILE
static inline void boot_profile_emit(sstring stage)
{
    rprintf("hydrogen-startup %s monotonic_us=%lu\n", stage,
            usec_from_timestamp(now(CLOCK_ID_MONOTONIC_RAW)));
}
#define boot_profile(stage) boot_profile_emit(ss(stage))
#else
#define boot_profile(stage) do {} while (0)
#endif
