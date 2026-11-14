void serial_putchar(char c);
void serial_set_devbase(u64 devbase);
void serial_set_base(u64 phys_addr);

struct console_driver *pl011_console_init(kernel_heaps kh, void *base);
