#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
script=${BUILD_SH:-$root/images/nanos-helium/build.sh}
tmp=$root/.work/tmp/nanos-image-publish
sandbox=$tmp/sandbox
harness=$tmp/pack.sh

fail() { echo "tests/build/nanos-image-publish.sh: $*" >&2; exit 1; }

[ -f "$script" ] || fail "no $script"

rm -rf "$tmp"
mkdir -p "$sandbox"

images=$sandbox/.work/images
work=$images/nanos-helium
out=$images/nanos-helium.img
kernel_out=$images/nanos-helium.kernel.img
profile_out=$images/nanos-helium.profile.img
tree=$sandbox/.work/runtime/nanos/hydrogen
nano_out=$tree/nanos/output

mkdir -p "$tree/dev" "$nano_out/platform/pc/bin" "$nano_out/platform/pc/boot" "$nano_out/tools/bin"
printf 'commit\n' >"$tree/COMMIT"
printf 'kernel-new\n' >"$nano_out/platform/pc/bin/kernel.img"
printf 'boot\n' >"$nano_out/platform/pc/boot/boot.img"

cat >"$tree/dev/image.sh" <<'STUB'
#!/bin/sh
out=
while [ $# -gt 0 ]; do
    case $1 in
    -o) out=$2; shift 2 ;;
    *) shift ;;
    esac
done
printf 'partial\n' >"$out"
[ "${FAIL:-}" != image ] || exit 1
printf 'base\n' >"$out"
printf 'manifest\n' >"$out.manifest"
printf 'reproduce\n' >"$out.reproduce"
STUB

cat >"$nano_out/tools/bin/mkfs" <<'STUB'
#!/bin/sh
profile=0
for arg in "$@"; do
    [ "$arg" != -e ] || profile=1
    dest=$arg
done
printf 'partial\n' >"$dest"
if [ "$profile" = 1 ]; then
    [ "${FAIL:-}" != mkfs-profile ] || exit 1
    printf 'profile-new\n' >"$dest"
else
    [ "${FAIL:-}" != mkfs-image ] || exit 1
    [ "$(cat)" = manifest ] || exit 2
    printf 'image-new\n' >"$dest"
fi
STUB
chmod +x "$tree/dev/image.sh" "$nano_out/tools/bin/mkfs"

{
    echo 'set -eu'
    echo "root=$sandbox"
    echo 'force=0'
    sed -n -E '/^(runtime|tree|images|work|out|kernel_out|profile_out|nano_out|kernel|boot|mkfs|target|rootfs|guest)=/p' "$script"
    sed -n -E '/^(fail|sha|digest)\(\) /p' "$script"
    sed -n '/^stamp_valid() {/,/^}/p;/^write_stamp() {/,/^}/p' "$script"
    echo 'pack_identity() { echo pack; }'
    echo 'version=test'
    sed -n '/^image_stamp=/,$p' "$script"
} >"$harness"
for name in stamp_valid write_stamp image_stamp; do
    rg -q "$name" "$harness" || fail "cannot extract $name from $script"
done

new_image=$tmp/expect.img
new_profile=$tmp/expect.profile
new_kernel=$tmp/expect.kernel
printf 'image-new\n' >"$new_image"
printf 'profile-new\n' >"$new_profile"
printf 'kernel-new\n' >"$new_kernel"

seed() {
    mkdir -p "$work"
    printf 'old-image\n' >"$out"
    printf 'old-kernel\n' >"$kernel_out"
    printf 'old-profile\n' >"$profile_out"
    printf 'old-manifest\n' >"$out.manifest"
    printf 'old-reproduce\n' >"$out.reproduce"
    printf 'old-stamp\n' >"$work/image.stamp"
    rm -rf "$tmp/old"
    mkdir -p "$tmp/old"
    for file in "$out" "$kernel_out" "$profile_out" "$out.manifest" "$out.reproduce" "$work/image.stamp"; do
        cp "$file" "$tmp/old/$(basename "$file")"
    done
    find "$sandbox" | LC_ALL=C sort >"$tmp/listing"
}

previous() {
    for file in "$out" "$kernel_out" "$profile_out" "$out.manifest" "$out.reproduce" "$work/image.stamp"; do
        cmp -s "$tmp/old/$(basename "$file")" "$file" || return 1
    done
}

check_listing() {
    find "$sandbox" | LC_ALL=C sort >"$tmp/after"
    cmp -s "$tmp/listing" "$tmp/after" || fail "$1: files differ from before:
$(diff "$tmp/listing" "$tmp/after")"
}

for point in image mkfs-image mkfs-profile; do
    seed
    status=0
    output=$(FAIL=$point sh "$harness" 2>&1) || status=$?
    [ "$status" != 0 ] || fail "$point: the build succeeded despite the injected failure"
    previous || fail "$point: the previous image, kernel or profile template changed (exit $status):
$output"
    check_listing "$point"
    echo "ok: failure at $point keeps the previous image, kernel and profile template"
done

seed
output=$(sh "$harness" 2>&1) || fail "success run failed:
$output"
printf '%s\n' "$output" | rg -qxF 'image: built' || fail "want 'image: built' in:
$output"
cmp -s "$new_image" "$out" || fail "$out was not replaced"
cmp -s "$new_kernel" "$kernel_out" || fail "$kernel_out was not replaced"
cmp -s "$new_profile" "$profile_out" || fail "$profile_out was not replaced"
check_listing "success"
echo "ok: success replaces the image, kernel and profile template"
