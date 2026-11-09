ROOT:=		$(abspath ../..)
include $(ROOT)/dev/toolchain.lock
CC:=		$(ROOT)/.work/toolchain/$(LLVM_DIR)/bin/clang
SYSROOT:=	$(ROOT)/.work/sysroot
OUT:=		$(ROOT)/.work/build/mini_chromium
CFLAGS:=	--sysroot=$(SYSROOT) -O2 -g -Wall -Wextra -Werror

STATIC_FIXTURES:=	hello_static hello_pthread crash silent_exit hang duplicate_result
DYNAMIC_FIXTURES:=	hello_stdio

all: $(addprefix $(OUT)/fixtures/,$(STATIC_FIXTURES) $(DYNAMIC_FIXTURES))

$(addprefix $(OUT)/fixtures/,$(STATIC_FIXTURES)): $(OUT)/fixtures/%: fixtures/%.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -static -pthread $< -o $@

$(addprefix $(OUT)/fixtures/,$(DYNAMIC_FIXTURES)): $(OUT)/fixtures/%: fixtures/%.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -pthread $< -o $@

.PHONY: all
