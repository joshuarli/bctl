#include <elf.h>
#include <link.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <unistd.h>

extern void _start(void);

int main(int argc, char **argv)
{
	if (argc != 2 || strcmp(argv[1], "dynamic") != 0)
		return 10;
	const char *marker = getenv("EXEC_CONTRACT");
	if (!marker || strcmp(marker, "helper") != 0)
		return 11;
	if (getauxval(AT_BASE) == 0)
		return 12;
	if (getauxval(AT_ENTRY) != (unsigned long)_start)
		return 13;
	const char *execfn = (const char *)getauxval(AT_EXECFN);
	if (!execfn || strcmp(execfn, argv[0]) != 0)
		return 14;
	const ElfW(Phdr) *ph = (const ElfW(Phdr) *)getauxval(AT_PHDR);
	unsigned long phnum = getauxval(AT_PHNUM);
	int interp = 0;
	for (unsigned long i = 0; ph && i < phnum; i++)
		if (ph[i].p_type == PT_INTERP)
			interp = 1;
	if (!interp || getauxval(AT_PHENT) != sizeof(ElfW(Phdr)))
		return 15;
	return 0;
}
