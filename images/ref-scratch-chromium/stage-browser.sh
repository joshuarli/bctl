#!/bin/sh
# Install the one Helium-layout archive in ARCHIVE_DIR unchanged at ROOTFS/opt/browser; link its executable.
# Usage: stage-browser.sh ARCHIVE_DIR ROOTFS
set -eu
archives=${1:?usage: stage-browser.sh ARCHIVE_DIR ROOTFS}
rootfs=${2:?usage: stage-browser.sh ARCHIVE_DIR ROOTFS}

fail() { echo "stage-browser.sh: $*" >&2; exit 1; }

set -- "$archives"/*.tar.xz
[ $# = 1 ] && [ -f "$1" ] || fail "want exactly one *.tar.xz in $archives, found: $*"
archive=$1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xJpf "$archive" -C "$work"
set -- "$work"/*
[ $# = 1 ] && [ -d "$1" ] || fail "$archive must hold exactly one top-level directory"
top=$1
executable=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["executable"])' "$top/package-manifest.json")
case $executable in ''|*/*|.*) fail "bad executable name $executable in $archive" ;; esac
[ "$(head -c 4 "$top/$executable" | od -An -c | tr -d ' ')" = '177ELF' ] || fail "$executable in $archive is not an ELF file"
[ -d "$top/lib" ] && [ -f "$top/fontconfig/fonts.conf" ] || fail "$archive lacks lib/ or fontconfig/fonts.conf"

mkdir -p "$rootfs/opt" "$rootfs/usr/local/bin"
mv "$top" "$rootfs/opt/browser"
ln -s "/opt/browser/$executable" "$rootfs/usr/local/bin/browser"
