# Shared graphics dependencies

Tinyland, seatd, and seatd-protocol are owned by
https://github.com/laputa-systems/tinyland. The image builders consume the
commit in `guest/tinyland.rev` through a named Docker Git context.

Send shared fixes, adjustments, and features directly upstream. Verify them
there, then update this pin and qualify the affected bctl image. Do not vendor
these crates or keep consumer-only patches. Appliance supervision, image
assembly, and bctl-specific proofs remain here.

Do not run formatters or change pinned Rust toolchains.
