ROOT:=		$(abspath ../..)
include $(ROOT)/dev/toolchain.lock
CC:=		$(ROOT)/.work/toolchain/$(LLVM_DIR)/bin/clang
SYSROOT:=	$(ROOT)/.work/sysroot
OUT:=		$(ROOT)/.work/build/external
CFLAGS:=	--sysroot=$(SYSROOT) -O2 -g -Wall -Wextra -Werror -Wno-unused-function -static

CASES:=		dup fcntl lseek fork exec wait socket_unix_cmsg socket_unix_peercred

all: $(addprefix $(OUT)/gvisor_,$(CASES))

$(addprefix $(OUT)/gvisor_,$(CASES)): $(OUT)/gvisor_%: %.c harness.h
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) $< -o $@

.PHONY: all
