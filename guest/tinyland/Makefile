QEMU ?= qemu-system-x86_64

.PHONY: smoke seat drm-proof wayland-pixels check test audit drm-parity negative

check:
	cargo check --locked

test:
	cargo test --locked
	python3 -B harness/test-qmp.py
	python3 -B harness/test-guest-lock.py

drm-proof:
	bash harness/drm-proof.sh

wayland-pixels:
	bash harness/wayland-pixels.sh

smoke:
	bash harness/smoke.sh

seat:
	bash harness/build-guest.sh 2>&1 | tail -n 5
	bash harness/seat-proof.sh

audit:
	bash harness/audit.sh

drm-parity:
	bash harness/drm-parity.sh

negative:
	bash harness/negative-runtime.sh
