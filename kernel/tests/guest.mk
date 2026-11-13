include $(ROOT)/dev/toolchain.lock
SYSROOT:=	$(ROOT)/.work/sysroot
ifeq ($(shell uname -m),aarch64)
CC:=		$(SYSROOT)/bin/musl-gcc
GUEST_FLAGS:=	-Wl,--dynamic-linker=/lib/ld-musl-aarch64.so.1 -Wl,-Ttext-segment=0x200000
else
CC:=		$(ROOT)/.work/toolchain/$(LLVM_DIR)/bin/clang
GUEST_FLAGS:=	--sysroot=$(SYSROOT)
endif
