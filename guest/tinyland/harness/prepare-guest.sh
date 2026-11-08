#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve())' "$1")"
case "$ROOT" in "$PWD"/.work/*) ;; *) echo 'guest root must be inside repository .work' >&2; exit 2 ;; esac
if [ -L "$ROOT" ]; then echo 'guest root must not be a symlink' >&2; exit 2; fi
EXCLUDE="$(git rev-parse --git-path info/exclude)"
if ! rg -q '^/?\.work/$' "$EXCLUDE"; then printf '\n.work/\n' >> "$EXCLUDE"; fi
mkdir -p .work/dl/apk .work/tmp
export TMPDIR="$PWD/.work/tmp"
MIRROR="${ALPINE_MIRROR:-https://dl-cdn.alpinelinux.org/alpine}"
TARBALL=alpine-minirootfs-3.20.3-x86_64.tar.gz
if [ ! -f ".work/dl/$TARBALL" ]; then
  curl --fail --location "$MIRROR/v3.20/releases/x86_64/$TARBALL" --output ".work/dl/$TARBALL.part"
  mv ".work/dl/$TARBALL.part" ".work/dl/$TARBALL"
fi
printf '%s  %s\n' d4e6fd67dcf75e40c451560ac7265166c2b72a0f38ddc9aae756a7de3d1efa0c ".work/dl/$TARBALL" | sha256sum -c -
if [ ! -f "$ROOT/etc/alpine-release" ]; then
  mkdir -p "$ROOT"
  tar -xzf ".work/dl/$TARBALL" -C "$ROOT"
fi
if [ "$(cat "$ROOT/etc/alpine-release")" != 3.20.3 ]; then echo 'wrong guest rootfs release' >&2; exit 1; fi
APKS=()
while read -r DIGEST PACKAGE; do
  if [[ ! "$DIGEST" =~ ^[a-f0-9]{64}$ || ! "$PACKAGE" =~ ^(main|community)/[a-zA-Z0-9._+-]+\.apk$ ]]; then
    echo 'invalid guest package lock entry' >&2; exit 1
  fi
  TARGET="$PWD/.work/dl/apk/$PACKAGE"
  if [ ! -f "$TARGET" ]; then
    REPOSITORY="${PACKAGE%%/*}"
    mkdir -p "${TARGET%/*}"
    curl --fail --location "$MIRROR/v3.20/$REPOSITORY/x86_64/${PACKAGE#*/}" --output "$TARGET.part"
    mv "$TARGET.part" "$TARGET"
  fi
  APKS+=("$TARGET")
done < guest/packages.sha256
if [ "${#APKS[@]}" -eq 0 ]; then echo 'empty guest package lock' >&2; exit 1; fi
(cd .work/dl/apk && sha256sum -c "$OLDPWD/guest/packages.sha256")
apk --keys-dir "$ROOT/etc/apk/keys" verify "${APKS[@]}"
apk --root "$ROOT" --repositories-file /dev/null --no-network --keys-dir "$ROOT/etc/apk/keys" \
  --initdb --arch x86_64 --usermode add "${APKS[@]}"
echo GUEST_PACKAGES_VERIFIED
