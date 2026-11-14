#include <dlfcn.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

typedef int32_t (*add_disk_fn)(uint32_t ctx_id, const char *block_id, const char *disk_path, bool read_only);

int32_t krun_create_ctx(void);
int32_t krun_set_vm_config(uint32_t ctx_id, uint8_t num_vcpus, uint32_t ram_mib);
int32_t krun_add_serial_console_default(uint32_t ctx_id, int input_fd, int output_fd);
int32_t krun_add_disk(uint32_t ctx_id, const char *block_id, const char *disk_path, bool read_only);
int32_t krun_set_firmware(uint32_t ctx_id, const char *firmware_path);
int32_t krun_add_virtiofs(uint32_t ctx_id, const char *c_tag, const char *c_path);
int32_t krun_start_enter(uint32_t ctx_id);

int main(int argc, char **argv)
{
	if (argc != 5 && argc != 7) {
		fprintf(stderr, "usage: krun-run DISK FIRMWARE CPUS MEMORY_MIB [VIRTIOFS_TAG VIRTIOFS_DIR]\n");
		return 2;
	}
	int cpus = atoi(argv[3]), memory = atoi(argv[4]);
	if (cpus < 1 || cpus > 255 || memory < 1) {
		fprintf(stderr, "krun-run: bad CPU count or memory size\n");
		return 2;
	}
	int console[2];
	if (pipe(console)) {
		perror("krun-run: console pipe");
		return 1;
	}
	add_disk_fn add_disk = (add_disk_fn)dlsym(RTLD_DEFAULT, "krun_add_disk_dax");
	if (!add_disk)
		add_disk = krun_add_disk;
	int32_t ctx = krun_create_ctx(), r;
	if (ctx < 0 || (r = krun_set_vm_config((uint32_t)ctx, (uint8_t)cpus, (uint32_t)memory)) < 0 ||
	    (r = krun_add_serial_console_default((uint32_t)ctx, console[0], STDOUT_FILENO)) < 0 ||
	    (r = add_disk((uint32_t)ctx, "root", argv[1], false)) < 0 ||
	    (r = krun_set_firmware((uint32_t)ctx, argv[2])) < 0 ||
	    (argc == 7 && (r = krun_add_virtiofs((uint32_t)ctx, argv[5], argv[6])) < 0)) {
		fprintf(stderr, "krun-run: libkrun configuration failed\n");
		return 1;
	}
	r = krun_start_enter((uint32_t)ctx);
	fprintf(stderr, "krun-run: libkrun could not start the VM: %d\n", r);
	return 1;
}
