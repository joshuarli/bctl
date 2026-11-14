# Shared graphics dependencies

Tinyland, seatd, and seatd-protocol are owned by
https://github.com/laputa-systems/tinyland. The image builders consume the
commit in `guest/tinyland.rev` through a named Docker Git context.

Send shared fixes, adjustments, and features directly upstream. Verify them
there, then update this pin and qualify the affected bctl image. Do not vendor
these crates or keep consumer-only patches. Appliance supervision, image
assembly, and bctl-specific proofs remain here.

# Kernel

`kernel/` is Hydrogen, bctl's Nanos fork. Kernel mechanisms, platform
adaptation, and kernel qualification live there. Read
[docs/kernel/development.md](docs/kernel/development.md) before changing it:
it holds the rules kernel changes follow and what counts as a pass. Commit
kernel changes before running `runtime/build-nanos*.sh`, which build only the
committed `kernel/` tree.

The active kernel scope includes [incremental Rust adoption](docs/kernel/rust-adoption.md),
a companion to the main campaign. Start with the mixed-language foundation,
then the Rust ownership repairs for BCTL-15 and BCTL-17. Preserve their IDs and
open status until their evidence gates pass; a Rust port is not itself a fix.
Migrate ownership domains behind audited C boundaries, not raw-pointer syntax.
Keep unrelated kernel qualification and performance work active.

# Performance experiments

Before trying a performance change, check [docs/optimizations.md](docs/optimizations.md)
for earlier attempts. Record every attempt there when it finishes, including
ones that are rejected, inconclusive, or rule out a suspected cause.

Do not run formatters or change pinned Rust toolchains.

## Graphics performance execution

For headless performance attribution and parallel headed damage work, follow [the graphics campaign](docs/CAMPAIGN-GRAPHICS-PERFORMANCE.md), BCTL-21/BCTL-22 in [TODO.md](TODO.md), and upstream tinyland TINY-14. The existing two-second startup trace is not a warm-operation trace. Preserve existing kernel campaigns and avoid duplicating tinyland compositor ownership in bctl.
