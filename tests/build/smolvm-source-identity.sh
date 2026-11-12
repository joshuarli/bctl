#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$root/.work/tmp/smolvm-source-identity
updater=$root/scripts/update_smolvm.py

fail() { echo "tests/build/smolvm-source-identity.sh: $*" >&2; exit 1; }
verify() { python3 -I "$updater" --repo-root "$1" --verify-source 2>&1; }

mkdir -p "$root/.work/tmp"
rm -rf "$tmp"
trap 'rm -rf "$tmp"' EXIT INT TERM

out=$(verify "$root") || fail "the checkout fails its own identity check:\n$out"
echo "checkout: $out"

mkdir -p "$tmp"
cp -a "$root/third_party" "$tmp/third_party"
victim=$tmp/third_party/smolvm/Cargo.toml
[ -f "$victim" ] || fail "no vendored Cargo.toml to modify"

out=$(verify "$tmp") || fail "the copy fails its identity check before any change:\n$out"

echo "# changed" >> "$victim"
if out=$(verify "$tmp"); then fail "a modified vendored file was accepted"; fi
echo "modified file: $out"
cp "$root/third_party/smolvm/Cargo.toml" "$victim"
verify "$tmp" >/dev/null || fail "the restored copy was rejected"

chmod +x "$victim"
if out=$(verify "$tmp"); then fail "a changed executable bit was accepted"; fi
echo "changed mode: $out"
chmod -x "$victim"
verify "$tmp" >/dev/null || fail "the restored mode was rejected"

echo "extra file" > "$tmp/third_party/smolvm/stray.txt"
if out=$(verify "$tmp"); then fail "an extra vendored file was accepted"; fi
echo "extra file: $out"
rm "$tmp/third_party/smolvm/stray.txt"
verify "$tmp" >/dev/null || fail "the copy without the extra file was rejected"

echo "ok"
