#!/bin/sh
set -eu

image=alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
packages="build-base=0.5-r3 ca-certificates cargo=1.87.0-r1 curl file patch pkgconf rust=1.87.0-r1 libseccomp-dev libseccomp-static=2.6.1-r0 libcap-ng-dev libcap-ng-static=0.8.5-r0 linux-headers"
vhost_vsock_version=0.3.0
vhost_vsock_sha256=52ce95d81af89ad693d067c650e42df0f217addfbc96cc4820f7c62eb4a28b13
vhost_vsock_patches="vhost-device-vsock-0001-forward-guest-half-close.patch vhost-device-vsock-0002-keep-vring-worker-before-rings-ready.patch"
virtiofsd_version=1.14.0
virtiofsd_sha256=7051accc9cf2816e25945c8216e6d4be2b4e75fc5e77559cd7eed5e400245cc1
root=$(cd "$(dirname "$0")/.." && pwd)

fail() { printf 'release/build-linux-runtime.sh: %s\n' "$*" >&2; exit 1; }

out=''
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || fail "missing value for $1"
    case $1 in
        --out) [ -z "$out" ] || fail 'duplicate --out'; out=$2 ;;
        *) fail "unknown option $1" ;;
    esac
    shift 2
done
[ -n "$out" ] || fail '--out is required'
command -v docker >/dev/null || fail 'docker is not installed'
if [ -e "$out" ] || [ -L "$out" ]; then
    [ -d "$out" ] && [ ! -L "$out" ] || fail 'out must be an empty directory'
    [ -z "$(ls -A "$out")" ] || fail 'out must be empty'
else
    mkdir -p "$out" || fail 'cannot create out'
fi
out=$(cd "$out" && pwd)
host_owner=
[ "$(uname -s)" = Linux ] && host_owner=$(id -u):$(id -g)

docker run --rm --platform linux/amd64 \
    -v "$root/runtime/patches:/patches:ro" \
    -v "$out:/out" \
    -e PACKAGES="$packages" \
    -e VSOCK_VERSION="$vhost_vsock_version" \
    -e VSOCK_SHA256="$vhost_vsock_sha256" \
    -e VSOCK_PATCHES="$vhost_vsock_patches" \
    -e VIRTIOFSD_VERSION="$virtiofsd_version" \
    -e VIRTIOFSD_SHA256="$virtiofsd_sha256" \
    -e HOST_OWNER="$host_owner" \
    "$image" sh -eu -c '
        trap "test -z \$HOST_OWNER || chown -R \$HOST_OWNER /out" EXIT
        apk add --no-cache $PACKAGES >/dev/null

        fetch() {
            curl --proto "=https" --tlsv1.2 -fsSL -o "/dl/$1-$2.crate" "https://static.crates.io/crates/$1/$1-$2.crate"
            printf "%s  %s\n" "$3" "/dl/$1-$2.crate" | sha256sum -c -s || { echo "$1 $2 does not match its crates.io checksum" >&2; exit 1; }
            tar -xzf "/dl/$1-$2.crate" -C /src
        }
        mkdir -p /dl /src
        fetch vhost-device-vsock "$VSOCK_VERSION" "$VSOCK_SHA256"
        fetch virtiofsd "$VIRTIOFSD_VERSION" "$VIRTIOFSD_SHA256"
        for patch in $VSOCK_PATCHES; do
            patch -p1 -d "/src/vhost-device-vsock-$VSOCK_VERSION" --batch --forward --silent -i "/patches/$patch"
        done

        export SOURCE_DATE_EPOCH=0
        export RUSTFLAGS="-C target-feature=+crt-static -C relocation-model=static --remap-path-prefix=/src=/src --remap-path-prefix=/root/.cargo=/cargo"
        export LIBSECCOMP_LINK_TYPE=static LIBSECCOMP_LIB_PATH=/usr/lib
        export LIBCAPNG_LINK_TYPE=static LIBCAPNG_LIB_PATH=/usr/lib
        for crate in vhost-device-vsock-$VSOCK_VERSION virtiofsd-$VIRTIOFSD_VERSION; do
            (cd "/src/$crate" && cargo build --locked --release --target x86_64-alpine-linux-musl)
        done
        for name in vhost-device-vsock:vhost-device-vsock-$VSOCK_VERSION virtiofsd:virtiofsd-$VIRTIOFSD_VERSION; do
            install -m 755 "/src/${name#*:}/target/x86_64-alpine-linux-musl/release/${name%%:*}" "/out/${name%%:*}"
            strip --strip-all "/out/${name%%:*}"
        done
    ' || fail 'container build failed'

for name in vhost-device-vsock virtiofsd; do
    [ -s "$out/$name" ] && [ -x "$out/$name" ] || fail "build produced no $out/$name"
done
