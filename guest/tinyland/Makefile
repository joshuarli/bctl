.PHONY: check test audit proof seat

check:
	cargo check --locked

test:
	cargo test --locked

audit:
	bash harness/audit.sh

proof:
	sh ../harness/smolvm-proof.sh

seat: proof
