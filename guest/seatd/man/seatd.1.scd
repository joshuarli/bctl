seatd(1)

# NAME

seatd - Linux seat management daemon

# SYNOPSIS

*seatd* [*-h*] [*-v*] [*-n* fd] [*-u* user] [*-g* group] [*-l* level] [*-z*]

# DESCRIPTION

seatd is a foreground daemon that provides one seat, *seat0*, over a Unix
domain socket. Clients use the existing libseat seatd protocol. The default
socket path is */run/seatd.sock* on Linux.

The daemon uses socket permissions to admit clients. Any process that can
connect can make seat requests and request supported devices. Seatd does not
authenticate users or provide login, power, or supervision management. See
*doc/runtime.md* for operating details and recovery guidance.

# OPTIONS

*-h*
	Show help and exit.

*-v*
	Show the version and exit.

*-n* fd
	Write one newline to fd after the socket is ready, then close fd. This
	readiness notification is compatible with s6.

*-u* user
	Set the socket owner. When either *-u* or *-g* is given, seatd sets the
	socket mode to 0770. This option cannot be combined with *-z*.

*-g* group
	Set the socket group. When either *-u* or *-g* is given, seatd sets the
	socket mode to 0770. This option cannot be combined with *-z*.

*-l* level
	Set the log level to *debug*, *info*, *error*, or *silent*. The default is
	*info*.

*-z*
	Refuse to start if anything already exists at the socket path. This
	option cannot be combined with *-u* or *-g*. Without *-z*, seatd probes
	the existing socket: a live daemon is never stolen from, a non-socket
	file is never removed, and only a stale socket left by a dead daemon is
	reclaimed before binding.

# ENVIRONMENT

*SEATD_SOCK*
	Set the socket path. The default is */run/seatd.sock*.

*SEATD_VTBOUND*
	Enable VT binding only when the value is exactly *1*. The default is enabled
	when the variable is unset. Every other value disables VT binding. VT-bound
	operation has been qualified in a disposable x86_64 virtio guest. See
	*harness/VM.md* for the tested scope.

# FILES

*/run/seatd.sock*
	Default Linux socket path.

# SEE ALSO

*doc/runtime.md*
