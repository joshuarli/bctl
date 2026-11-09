#!/bin/bash
set -euo pipefail

repo=/repo
src=$repo/third_party/smolvm
work=/work
dl=$work/downloads
deps=$work/deps
build=$work/build
out=$work/bin
rootfs=$work/rootfs
rust_target=aarch64-unknown-linux-musl
c_target=aarch64-linux-musl
clang=/opt/llvm/bin/clang
sysroot=/opt/musl
rust_toolchain=${RUST_TOOLCHAIN:?}

llvm_url=https://github.com/laputa-systems/llvm-prebuilt-musl/releases/download/llvm-musl-23.1.5-3afbb74/clang%2Bllvm-23.1.5-aarch64-linux-musl.tar.xz
llvm_sha=ca1c593163ac37d6656a5fddebec4c2714050c7345d9326e5afa8ea46949cede
musl_version=1.2.6
musl_sha=d585fd3b613c66151fc3249e8ed44f77020cb5e6c1e635a616d3f9f82460512a
busybox_version=1.38.0
busybox_sha=34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2
tar_version=1.35
tar_sha=4d62ff37342ec7aed748535323930c7cf94acf71c3591882b26a7ea50f3edc16
e2fsprogs_version=1.47.4
e2fsprogs_sha=da274408bebbfd13a5a2fc3cfc66e3ffff17c48534673aa67f88d49b99123b96
util_linux_version=2.42.4
util_linux_sha=fbd62a100ab7bb8746ba0661255c3c48185b1e9021507c624da01fbc696330ec
json_c_version=0.19-20260627
json_c_sha=704927172443309a8efeb162060bb215548e1286e5568514007dd2cc35a0a164
libcap_version=2.77
libcap_sha=5e339f5ab434bf5b7979f65437ca24942ddcd6e6a3e57347370cd791bc0ea51c
libseccomp_version=2.6.1
libseccomp_sha=501f66c667225d53791b97e1d7cf85ab764c297d04881f60f38f451c4b0ee1be
argp_version=1.5.0
argp_sha=c29eae929dfebd575c38174f2c8c315766092cec99a8f987569d0cad3c6d64f6
crun_version=1.30.1
crun_sha=99d036d95113552b6df66c7c879432835504bf0e525fc0372745071df129527b
curl_version=8.23.0
curl_sha=8dadd7d2536a314f982ad05a651b713f426bf7d02cc2c76cffb9796f2218e6c5
ca_bundle_sha=427c064216761a1e72a8b30f0d954d08cb6e4435a82091fb865afd20c8497285
go_version=1.26.10
go_sha=3f4227ce7b206a513e794f1393adb473a07b46e94d18bd15d1c684bbb7319797
crane_version=0.22.1

cflags="--target=$c_target --sysroot=$sysroot -O2"

fetch() {
    url=$1 sha=$2 name=$3
    [ -s "$dl/$name" ] || curl -fsSL --retry 3 "$url" -o "$dl/$name"
    echo "$sha  $dl/$name" | sha256sum -c -
}

run() {
    name=$1
    shift
    if ! "$@" >"$build/$name.log" 2>&1; then
        echo "$name failed; tail of $build/$name.log:" >&2
        tail -n 40 "$build/$name.log" >&2
        exit 1
    fi
}

mkdir -p "$dl" "$deps" "$build" "$out"

if [ ! -x /opt/llvm/bin/clang ]; then
    fetch "$llvm_url" "$llvm_sha" llvm.tar.xz
    rm -rf /opt/llvm
    mkdir -p /opt/llvm
    tar -xJf "$dl/llvm.tar.xz" -C /opt/llvm --strip-components=1
fi

if [ ! -f "$sysroot/usr/include/stdio.h" ]; then
    fetch "https://musl.libc.org/releases/musl-$musl_version.tar.gz" "$musl_sha" musl.tar.gz
    rm -rf "$build/musl-$musl_version"
    tar -xzf "$dl/musl.tar.gz" -C "$build"
    (cd "$build/musl-$musl_version" && ./configure --prefix=/usr --disable-shared >/dev/null && make install-headers prefix=/usr DESTDIR="$sysroot" >/dev/null)
fi
for d in linux asm asm-generic mtd; do
    [ -d "/usr/include/$d" ] && cp -a "/usr/include/$d" "$sysroot/usr/include/"
done

fetch "https://github.com/json-c/json-c/releases/download/json-c-$json_c_version/json-c-${json_c_version%-*}-nodoc.tar.gz" "$json_c_sha" json-c.tar.gz
rm -rf "$build/json-c" && mkdir -p "$build/json-c"
tar -xzf "$dl/json-c.tar.gz" -C "$build/json-c" --strip-components=1
run json-c cmake -S "$build/json-c" -B "$build/json-c/out" \
    -DCMAKE_C_COMPILER="$clang" -DCMAKE_C_FLAGS="$cflags" \
    -DCMAKE_INSTALL_PREFIX="$deps" -DBUILD_SHARED_LIBS=OFF -DBUILD_STATIC_LIBS=ON \
    -DCMAKE_EXE_LINKER_FLAGS=-static -DBUILD_TESTING=OFF -DDISABLE_WERROR=ON
run json-c-build cmake --build "$build/json-c/out" -j "$(nproc)"
run json-c-install cmake --install "$build/json-c/out"

fetch "https://git.kernel.org/pub/scm/libs/libcap/libcap.git/snapshot/libcap-$libcap_version.tar.gz" "$libcap_sha" libcap.tar.gz
rm -rf "$build/libcap-$libcap_version"
tar -xzf "$dl/libcap.tar.gz" -C "$build"
run libcap make -C "$build/libcap-$libcap_version/libcap" -j "$(nproc)" \
    CC="$clang" CFLAGS="$cflags" AR=/opt/llvm/bin/llvm-ar RANLIB=/opt/llvm/bin/llvm-ranlib \
    GOLANG=no libcap.a
mkdir -p "$deps/lib" "$deps/include"
cp "$build/libcap-$libcap_version/libcap/libcap.a" "$deps/lib/"
cp -a "$build/libcap-$libcap_version/libcap/include/." "$deps/include/"

fetch "https://github.com/seccomp/libseccomp/releases/download/v$libseccomp_version/libseccomp-$libseccomp_version.tar.gz" "$libseccomp_sha" libseccomp.tar.gz
rm -rf "$build/libseccomp-$libseccomp_version"
tar -xzf "$dl/libseccomp.tar.gz" -C "$build"
run libseccomp-configure sh -c "cd '$build/libseccomp-$libseccomp_version' && ./configure --prefix='$deps' --disable-shared --enable-static CC='$clang' CFLAGS='$cflags' LDFLAGS='-static'"
run libseccomp-make make -C "$build/libseccomp-$libseccomp_version" -j "$(nproc)" LDFLAGS='-static -all-static'
run libseccomp-install make -C "$build/libseccomp-$libseccomp_version" install

fetch "https://github.com/argp-standalone/argp-standalone/archive/refs/tags/$argp_version.tar.gz" "$argp_sha" argp.tar.gz
rm -rf "$build/argp-$argp_version"
tar -xzf "$dl/argp.tar.gz" -C "$build"
mv "$build/argp-standalone-$argp_version" "$build/argp-$argp_version"
run argp-configure env CC="$clang" meson setup "$build/argp-$argp_version/out" "$build/argp-$argp_version" \
    --prefix="$deps" --default-library=static -Dc_args="$cflags -static" -Dc_link_args=-static
run argp-make meson compile -C "$build/argp-$argp_version/out"
run argp-install meson install -C "$build/argp-$argp_version/out"

fetch "https://github.com/containers/crun/releases/download/$crun_version/crun-$crun_version.tar.gz" "$crun_sha" crun.tar.gz
rm -rf "$build/crun-$crun_version"
tar -xzf "$dl/crun.tar.gz" -C "$build"
run crun-configure sh -c "cd '$build/crun-$crun_version' && ./configure --prefix='$deps' --disable-shared --enable-static --disable-systemd --disable-criu CC='$clang' CFLAGS='$cflags' LDFLAGS='-static -L$deps/lib' CPPFLAGS='-I$deps/include' PKG_CONFIG_PATH='$deps/lib/pkgconfig'"
run crun-make make -C "$build/crun-$crun_version" -j "$(nproc)" LDFLAGS='-static -all-static'
cp "$build/crun-$crun_version/crun" "$out/crun"

fetch "https://mirrors.edge.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v$e2fsprogs_version/e2fsprogs-$e2fsprogs_version.tar.gz" "$e2fsprogs_sha" e2fsprogs.tar.gz
rm -rf "$build/e2fsprogs-$e2fsprogs_version"
tar -xzf "$dl/e2fsprogs.tar.gz" -C "$build"
run e2fsprogs-configure sh -c "cd '$build/e2fsprogs-$e2fsprogs_version' && ./configure --prefix='$deps' --disable-nls --disable-elf-shlibs --disable-uuidd --disable-e2initrd-helper --disable-debugfs --disable-imager --disable-defrag --disable-tdb CC='$clang' CFLAGS='$cflags' LDFLAGS='-static'"
# e2fsprogs 1.47.4's lib/ss races itself under -j.
run e2fsprogs-make make -C "$build/e2fsprogs-$e2fsprogs_version"
for bin in misc/mke2fs e2fsck/e2fsck resize/resize2fs; do
    cp "$build/e2fsprogs-$e2fsprogs_version/$bin" "$out/$(basename "$bin")"
done

fetch "https://ftp.gnu.org/gnu/tar/tar-$tar_version.tar.xz" "$tar_sha" tar.tar.xz
rm -rf "$build/tar-$tar_version"
tar -xJf "$dl/tar.tar.xz" -C "$build"
run tar-configure sh -c "cd '$build/tar-$tar_version' && FORCE_UNSAFE_CONFIGURE=1 ./configure --prefix='$deps' --disable-nls --without-selinux --without-posix-acls --without-xattrs --disable-acl CC='$clang' CFLAGS='$cflags' LDFLAGS='-static'"
run tar-make make -C "$build/tar-$tar_version" -j "$(nproc)"
cp "$build/tar-$tar_version/src/tar" "$out/tar"

fetch "https://mirrors.edge.kernel.org/pub/linux/utils/util-linux/v${util_linux_version%.*}/util-linux-$util_linux_version.tar.xz" "$util_linux_sha" util-linux.tar.xz
rm -rf "$build/util-linux-$util_linux_version"
tar -xJf "$dl/util-linux.tar.xz" -C "$build"
run util-linux-configure sh -c "cd '$build/util-linux-$util_linux_version' && ./configure --prefix='$deps' --disable-shared --enable-static --disable-nls --disable-all-programs --enable-libblkid --enable-libmount --enable-fstrim --enable-nsenter --without-selinux --without-udev --without-cap-ng --without-systemd --without-python --disable-pylibmount --disable-liblastlog2 CC='$clang' CFLAGS='$cflags' LDFLAGS='-static'"
run util-linux-make make -C "$build/util-linux-$util_linux_version" -j "$(nproc)" LDFLAGS='-static -all-static'
cp "$build/util-linux-$util_linux_version/fstrim" "$out/fstrim"
cp "$build/util-linux-$util_linux_version/nsenter" "$out/nsenter"

fetch "https://curl.se/download/curl-$curl_version.tar.xz" "$curl_sha" curl.tar.xz
rm -rf "$build/curl-$curl_version"
tar -xJf "$dl/curl.tar.xz" -C "$build"
run curl-configure sh -c "cd '$build/curl-$curl_version' && ./configure --prefix='$deps' --disable-shared --enable-static --without-ssl --without-libidn2 --without-libpsl --without-nghttp2 --without-libssh2 --without-brotli --without-zstd --without-zlib --disable-ldap --disable-ldaps --disable-rtsp --disable-dict --disable-telnet --disable-tftp --disable-pop3 --disable-imap --disable-smb --disable-smtp --disable-gopher --disable-mqtt --disable-manual --disable-docs --disable-threaded-resolver CC='$clang' CFLAGS='$cflags' LDFLAGS='-static'"
run curl-make make -C "$build/curl-$curl_version" -j "$(nproc)" LDFLAGS='-static -all-static'
cp "$build/curl-$curl_version/src/curl" "$out/curl"

fetch "https://busybox.net/downloads/busybox-$busybox_version.tar.bz2" "$busybox_sha" busybox.tar.bz2
rm -rf "$build/busybox-$busybox_version"
tar -xjf "$dl/busybox.tar.bz2" -C "$build"
(cd "$build/busybox-$busybox_version" && make defconfig >/dev/null &&
    sed -i -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' \
        -e 's/^CONFIG_NANDWRITE=y$/# CONFIG_NANDWRITE is not set/' \
        -e 's/^CONFIG_NANDDUMP=y$/# CONFIG_NANDDUMP is not set/' \
        -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' .config &&
    make oldconfig >/dev/null </dev/null)
run busybox-make make -C "$build/busybox-$busybox_version" -j "$(nproc)" HOSTCC=cc CC="$clang $cflags" LDFLAGS=-static
cp "$build/busybox-$busybox_version/busybox" "$out/busybox"

fetch https://curl.se/ca/cacert.pem "$ca_bundle_sha" cacert.pem

export RUSTUP_HOME=$work/rustup CARGO_HOME=$work/cargo
export PATH="$CARGO_HOME/bin:$PATH"
if [ ! -x "$CARGO_HOME/bin/cargo" ] || ! "$CARGO_HOME/bin/rustup" run "$rust_toolchain" rustc --version >/dev/null 2>&1; then
    curl -fsSL https://sh.rustup.rs -o "$work/rustup-init.sh"
    RUSTUP_HOME="$RUSTUP_HOME" CARGO_HOME="$CARGO_HOME" sh "$work/rustup-init.sh" -y --profile minimal \
        --default-toolchain "$rust_toolchain" --component rust-src >/dev/null
    rm -f "$work/rustup-init.sh"
fi
RUSTUP_TOOLCHAIN=$rust_toolchain
export RUSTUP_TOOLCHAIN
export RUSTFLAGS="-Clinker=$clang -Clink-self-contained=no -Clink-arg=--target=$c_target -Clink-arg=--sysroot=$sysroot"
export CC_aarch64_unknown_linux_musl=$clang
export CFLAGS_aarch64_unknown_linux_musl="--target=$c_target --sysroot=$sysroot"
export AR_aarch64_unknown_linux_musl=/opt/llvm/bin/llvm-ar

rm -rf "$work/target"
run agent cargo +"$rust_toolchain" build -Zbuild-std=std,panic_abort --locked --profile release-small \
    -p smolvm-agent --manifest-path "$src/Cargo.toml" --target "$rust_target" --target-dir "$work/target"
run krun-init cargo +"$rust_toolchain" build -Zbuild-std=std,panic_abort --locked --release \
    --manifest-path "$src/libkrun/init/Cargo.toml" --target "$rust_target" --target-dir "$work/target"
run seatd cargo +"$rust_toolchain" build -Zbuild-std=std,panic_abort --locked --release \
    -p seatd --manifest-path "$repo/guest/Cargo.toml" --target "$rust_target" --target-dir "$work/target-seatd"
cp "$work/target/$rust_target/release-small/smolvm-agent" "$out/smolvm-agent"
cp "$work/target/$rust_target/release/krun-init" "$out/krun-init"
cp "$work/target-seatd/$rust_target/release/seatd" "$out/seatd"

fetch "https://go.dev/dl/go$go_version.linux-arm64.tar.gz" "$go_sha" go.tar.gz
rm -rf /opt/go
tar -xzf "$dl/go.tar.gz" -C /opt
GOROOT=/opt/go GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH=arm64 \
    GOPATH="$work/gopath" GOMODCACHE="$work/gomodcache" PATH="/opt/go/bin:$PATH" \
    /opt/go/bin/go install -trimpath -ldflags='-s -w' "github.com/google/go-containerregistry/cmd/crane@v$crane_version" \
    >"$build/crane.log" 2>&1 || { tail -n 40 "$build/crane.log" >&2; exit 1; }
if [ -f "$work/gopath/bin/linux_arm64/crane" ]; then
    cp "$work/gopath/bin/linux_arm64/crane" "$out/crane"
else
    cp "$work/gopath/bin/crane" "$out/crane"
fi

rm -rf "$rootfs"
mkdir -p "$rootfs"/bin "$rootfs"/sbin "$rootfs"/usr/bin "$rootfs"/usr/sbin "$rootfs"/usr/local/bin \
    "$rootfs"/etc/ssl/certs "$rootfs"/etc/init.d "$rootfs"/dev "$rootfs"/proc "$rootfs"/sys \
    "$rootfs"/tmp "$rootfs"/run/smolvm/virtiofs "$rootfs"/storage "$rootfs"/root \
    "$rootfs"/var/log "$rootfs"/var/lib "$rootfs"/var/tmp \
    "$rootfs"/mnt/overlay "$rootfs"/mnt/storage "$rootfs"/mnt/newroot "$rootfs"/mnt/rosetta

install -m 0755 "$out/busybox" "$rootfs/bin/busybox"
while IFS= read -r applet; do
    case " $applet " in
        ' tar '|' fstrim '|' nsenter '|' init '|' linuxrc ') continue ;;
    esac
    ln -s busybox "$rootfs/bin/$applet"
done < <("$out/busybox" --list)
ln -s /bin/busybox "$rootfs/usr/bin/env"

install -m 0755 "$out/tar" "$rootfs/bin/tar"
install -m 0755 "$out/curl" "$rootfs/usr/bin/curl"
install -m 0755 "$out/crun" "$rootfs/usr/bin/crun"
install -m 0755 "$out/seatd" "$rootfs/usr/bin/seatd"
install -m 0755 "$out/nsenter" "$rootfs/usr/bin/nsenter"
install -m 0755 "$out/fstrim" "$rootfs/sbin/fstrim"
install -m 0755 "$out/mke2fs" "$rootfs/sbin/mke2fs"
ln -s mke2fs "$rootfs/sbin/mkfs.ext4"
install -m 0755 "$out/e2fsck" "$rootfs/sbin/e2fsck"
ln -s e2fsck "$rootfs/sbin/fsck.ext4"
install -m 0755 "$out/resize2fs" "$rootfs/sbin/resize2fs"
install -m 0755 "$out/smolvm-agent" "$rootfs/usr/local/bin/smolvm-agent"
install -m 0755 "$out/crane" "$rootfs/usr/local/bin/crane"
ln -s /usr/local/bin/smolvm-agent "$rootfs/sbin/init"

printf 'nameserver 1.1.1.1\n' > "$rootfs/etc/resolv.conf"
printf '127.0.0.1 localhost\n::1 localhost\n' > "$rootfs/etc/hosts"
printf 'smolvm\n' > "$rootfs/etc/hostname"
printf 'root:x:0:0:root:/root:/bin/sh\n' > "$rootfs/etc/passwd"
printf 'root:x:0:root\n' > "$rootfs/etc/group"
install -m 0644 "$dl/cacert.pem" "$rootfs/etc/ssl/certs/ca-certificates.crt"
ln -s certs/ca-certificates.crt "$rootfs/etc/ssl/cert.pem"
ln -s /proc/mounts "$rootfs/etc/mtab"
ln -s ../run "$rootfs/var/run"
install -m 0644 "$build/e2fsprogs-$e2fsprogs_version/misc/mke2fs.conf" "$rootfs/etc/mke2fs.conf"

chmod 1777 "$rootfs/tmp" "$rootfs/var/tmp"
chmod -R a+rX "$rootfs"
chmod 1777 "$rootfs/tmp" "$rootfs/var/tmp"

bash "$repo/runtime/guest/check-purity.sh" "$rootfs" aarch64 /opt/llvm/bin
rm -rf "$work/krun-only"
mkdir -p "$work/krun-only"
cp "$out/krun-init" "$work/krun-only/krun-init"
bash "$repo/runtime/guest/check-purity.sh" "$work/krun-only" aarch64 /opt/llvm/bin

echo "guest rootfs built at $rootfs"
