#!/bin/sh
# Pack the installed Alpine chromium APK and its runtime closure as an archive in the Helium archive layout.
# Usage: standin-archive.sh OUT_DIR
set -eu
out=${1:?usage: standin-archive.sh OUT_DIR}
version=$(apk info -e -v chromium | sed 's/^chromium-//; s/-r[0-9]*$//')
name=chromium-$version-linux-aarch64-musl
work=$(mktemp -d)
root=$work/$name
trap 'rm -rf "$work"' EXIT

fail() { echo "standin-archive.sh: $*" >&2; exit 1; }

traced_dlopens="libgtk-3.so.0 libgdk-3.so.0 libgdk_pixbuf-2.0.so.0 libpangocairo-1.0.so.0 libpangoft2-1.0.so.0
libcairo-gobject.so.2 libepoxy.so.0 libXcursor.so.1 libXinerama.so.1 libwayland-client.so.0 libwayland-cursor.so.0
libwayland-egl.so.1 libwayland-server.so.0 libxcb-present.so.0 libxcb-randr.so.0 libxcb-sync.so.1
libxcb-xfixes.so.0 libxshmfence.so.1 libdrm_amdgpu.so.1 libLLVM.so.20.1 libSPIRV-Tools.so libelf.so.1
gbm/dri_gbm.so libsoftokn3.so libfreeblpriv3.so libfreebl3.so libnssckbi.so"

find_library() {
    for directory in /usr/lib /lib /usr/lib/pulseaudio; do
        [ -e "$directory/$1" ] && { echo "$directory/$1"; return 0; }
    done
    return 1
}

mkdir -p "$root/lib"
cp -a /usr/lib/chromium/. "$root/"
rm "$root/chromium-launcher.sh"

pending=""
for file in $(find "$root" -type f); do
    [ "$(head -c 4 "$file" | od -An -c | tr -d ' ')" = '177ELF' ] && pending="$pending $file"
done
for library in $(cd /usr/lib && ls libgallium-*.so) $traced_dlopens; do
    source=$(find_library "$library") || fail "dlopened library $library is not installed"
    cp -L "$source" "$root/lib/${library##*/}"
    pending="$pending $root/lib/${library##*/}"
done
while [ -n "$pending" ]; do
    set -- $pending
    file=$1
    shift
    pending="$*"
    for needed in $(readelf -d "$file" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'); do
        case $needed in libc.musl-*|ld-musl-*) continue ;; esac
        [ -e "$root/lib/$needed" ] || [ -e "$root/$needed" ] && continue
        source=$(find_library "$needed") || fail "${file##*/} needs $needed, which is not installed"
        cp -L "$source" "$root/lib/$needed"
        pending="$pending $root/lib/$needed"
    done
done

mkdir -p "$root/fonts" "$root/fontconfig/conf.d"
cp -a /usr/share/fonts/dejavu "$root/fonts/"
for conf in /etc/fonts/conf.d/*.conf; do cp -L "$conf" "$root/fontconfig/conf.d/"; done
cat > "$root/fontconfig/fonts.conf" <<'CONF'
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir prefix="relative">../fonts</dir>
  <cachedir prefix="xdg">fontconfig</cachedir>
  <include ignore_missing="yes">conf.d</include>
</fontconfig>
CONF
printf '{\n  "schema": 1,\n  "product": "chromium",\n  "product_version": "%s",\n  "architecture": "arm64",\n  "executable": "chromium"\n}\n' "$version" > "$root/package-manifest.json"

mkdir -p "$out"
tar -C "$work" -cJf "$out/$name.tar.xz" "$name"
echo "$out/$name.tar.xz"
