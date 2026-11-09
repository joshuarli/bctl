ROOT:=		$(abspath ../..)
include $(ROOT)/dev/toolchain.lock
CC:=		$(ROOT)/.work/toolchain/$(LLVM_DIR)/bin/clang
SYSROOT:=	$(ROOT)/.work/sysroot
OUT:=		$(ROOT)/.work/build/helium
CFLAGS:=	--sysroot=$(SYSROOT) -O2 -g -Wall -Wextra -Werror

all: $(OUT)/guest_init

$(OUT)/guest_init: guest_init.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -static $< -o $@

.PHONY: all
