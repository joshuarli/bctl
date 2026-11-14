#include <kernel.h>
#include <gpio.h>

#define PL061_GPIOIEV   0x40c
#define PL061_GPIOIE    0x410
#define PL061_GPIOIC    0x41c

static BSS_RO_AFTER_INIT u64 gpio_base;

void gpio_set_base(u64 phys_addr)
{
    gpio_base = DEVICE_BASE + (phys_addr & (DEV_MAP_SIZE - 1));
}

#define gpio_mmio_base (gpio_base ? gpio_base : mmio_base_addr(GPIO))

void gpio_irq_enable(u64 mask)
{
    mmio_write_32(gpio_mmio_base + PL061_GPIOIEV, mask);
    mmio_write_32(gpio_mmio_base + PL061_GPIOIE, mask);
}

void gpio_irq_clear(u64 mask)
{
    mmio_write_32(gpio_mmio_base + PL061_GPIOIC, mask);
}
