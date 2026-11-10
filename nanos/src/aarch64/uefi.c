#include <runtime.h>
#include <page.h>
#include <uefi.h>
#include <kernel_platform.h>

void uefi_arch_setup(heap general, heap aligned, uefi_arch_options options)
{
    options->load_to_physical = true;
}

static void dcache_clean_inval(u64 start, u64 end)
{
    u64 ctr;
    asm volatile("mrs %0, CTR_EL0" : "=r"(ctr));
    u64 line = 4ull << ((ctr >> 16) & 0xf);
    for (u64 p = start & ~(line - 1); p < end; p += line)
        asm volatile("dc civac, %0" :: "r"(p) : "memory");
    asm volatile("dsb sy" ::: "memory");
}

void uefi_start_kernel(void *image_handle, efi_system_table system_table, buffer kern_elf,
                       void *kern_entry)
{
    struct uefi_boot_params boot_params = {0};
    for (int i = 0; i < system_table->number_of_table_entries; i++) {
        efi_configuration_table table = &system_table->configuration_table[i];
        if (!runtime_memcmp(&table->guid, &uefi_acpi20_table, sizeof(table->guid))) {
            boot_params.acpi_rsdp = u64_from_pointer(table->table);
        } else if (!runtime_memcmp(&table->guid, &uefi_smbios_table, sizeof(table->guid))) {
            boot_params.smbios = u64_from_pointer(table->table);
        }
    }
    boot_params.efi_rt_svc = system_table->runtime_services;
    uefi_exit_bs(&boot_params.mem_map);

    u64 bp = u64_from_pointer(&boot_params);
    dcache_clean_inval(bp, bp + sizeof(boot_params));
    u64 map = u64_from_pointer(boot_params.mem_map.map);
    if (map)
        dcache_clean_inval(map, map + boot_params.mem_map.map_size);

    u64 phys_off = (u64_from_pointer(&boot_params) & ~(u64)PHYSMEM_BASE_MASK) - PHYSMEM_BASE;
    u64 ctr;
    asm volatile("mrs %0, CTR_EL0" : "=r"(ctr));

    register u64 x0 asm("x0") = 0;
    register u64 x1 asm("x1") = u64_from_pointer(&boot_params);
    register u64 sweep asm("x2") = INIT_PAGEMEM + phys_off;
    register u64 sweep_end asm("x3") =
        (u64_from_pointer(kern_entry) & ~MASK(PAGELOG_2M)) + 8 * MB;
    register u64 line asm("x4") = 4ull << ((ctr >> 16) & 0xf);
    register u64 entry asm("x5") = u64_from_pointer(kern_entry);
    asm volatile("msr SCTLR_EL1, xzr\n"
                 "isb\n"
                 "1: dc civac, %[p]\n"
                 "add %[p], %[p], %[step]\n"
                 "cmp %[p], %[end]\n"
                 "b.lo 1b\n"
                 "ic iallu\n"
                 "dsb sy\n"
                 "isb\n"
                 "br %[entry]\n"
                 : [p] "+r" (sweep)
                 : [end] "r" (sweep_end), [step] "r" (line), [entry] "r" (entry),
                   "r" (x0), "r" (x1)
                 : "memory");
    __builtin_unreachable();
}

physical map_with_complete(u64 v, physical p, u64 length, pageflags flags, status_handler complete)
{
    if (complete)
        apply(complete, STATUS_OK);
    return p;
}
