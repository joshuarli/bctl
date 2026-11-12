# Shared graphics ownership

Tinyland, seatd, and seatd-protocol are maintained in the independent
https://github.com/laputa-systems/tinyland workspace. Shared compositor and
seat-protocol fixes, adjustments, and features belong upstream there.
Consume verified pinned revisions through the appliance integration; do not
vendor those implementations or retain consumer-only patches.

Kernel mechanisms, platform adaptation, and kernel qualification remain here.
