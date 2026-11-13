ROOT:=		$(abspath ../..)
include $(ROOT)/tests/guest.mk
OUT:=		$(ROOT)/.work/build/helium
CFLAGS:=	$(GUEST_FLAGS) -O2 -g -Wall -Wextra -Werror

all: $(OUT)/guest_init

$(OUT)/guest_init: guest_init.c
	@mkdir -p $(@D)
	$(CC) $(CFLAGS) -static $< -o $@

.PHONY: all
