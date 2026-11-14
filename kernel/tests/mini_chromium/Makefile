ROOT:=		$(abspath ../..)
include $(ROOT)/tests/guest.mk
OUT:=		$(ROOT)/.work/build/mini_chromium
CFLAGS:=	$(GUEST_FLAGS) -O2 -g -Wall -Wextra -Werror

STATIC_FIXTURES:=	hello_static hello_pthread cpus_online crash silent_exit hang duplicate_result meminfo writev_iov wait_nochild low_user_va fork_isolation exec_self vm_isolation poll_xproc cow_stress kill_race exec_contract process_stress shmem_xproc ipc_xproc creds_procfs seccomp seccomp_arch userns pidns mmap_bad_fd scm_rights crashpad_cmsg session_signal netns unix_abstract netlink_route tcp_loopback unix_cow_read file_cow_read image_links write_from_mapped_file vsock_reconnect private_page_race inherited_listener virtiofs_files unmap_empty p9_close mprotect_merge
DYNAMIC_FIXTURES:=	hello_stdio exec_contract_helper
STATIC_FIXTURES+=	vsock_cow
STATIC_FIXTURES+=	p9_mutation
STATIC_FIXTURES+=	wakeup_latency
STATIC_FIXTURES+=	fsfile_lifetime
STATIC_FIXTURES+=	proc_fstat_exited
STATIC_FIXTURES+=	futex_requeue_race
STATIC_FIXTURES+=	epoll_cow

all: $(OUT)/mini_chromium $(OUT)/mini_chromium_raw $(addprefix $(OUT)/fixtures/,$(STATIC_FIXTURES) $(DYNAMIC_FIXTURES)) $(OUT)/fixtures/vm_isolation_dyn $(OUT)/fixtures/cow_stress_dyn

$(OUT)/mini_chromium: mini_chromium.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -Wno-sign-compare -pthread $< -o $@

$(OUT)/mini_chromium_raw: mini_chromium_raw.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -static -nostdlib -ffreestanding -fno-builtin -fno-stack-protector $< -o $@

$(addprefix $(OUT)/fixtures/,$(STATIC_FIXTURES)): $(OUT)/fixtures/%: fixtures/%.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -static -pthread $< -o $@

$(addprefix $(OUT)/fixtures/,$(DYNAMIC_FIXTURES)): $(OUT)/fixtures/%: fixtures/%.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -pthread $< -o $@

$(OUT)/fixtures/vm_isolation_dyn: fixtures/vm_isolation.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -pthread $< -o $@

$(OUT)/fixtures/cow_stress_dyn: fixtures/cow_stress.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -pthread $< -o $@

.PHONY: all

$(OUT)/fixtures/resource_limits: $(ROOT)/nanos/test/runtime/resource_limits.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -static $< -o $@
