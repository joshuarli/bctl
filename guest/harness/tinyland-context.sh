#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
revision=$(cat "$root/tinyland.rev")
case "$revision" in
    *[!0-9a-f]*|'') echo 'invalid Tinyland revision' >&2; exit 1 ;;
esac
[ "${#revision}" -eq 40 ] || { echo 'Tinyland revision must be a full commit' >&2; exit 1; }
printf 'tinyland=https://github.com/laputa-systems/tinyland.git#%s\n' "$revision"
