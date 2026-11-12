ARG ROOTFS_BASE=alpine@sha256:e7a1a92a5bfeee40966aea60f0796b0e7917cc35591542701834f03a68fa3d18
FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS guest-build
RUN apk add --no-cache \
        bash build-base ca-certificates curl git mold musl-dev pkgconf \
        xkeyboard-config
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
 && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain none \
 && rm /tmp/rustup-init.sh \
 && rustup toolchain install nightly-2026-09-15 --profile minimal \
        --component rust-src --component clippy
WORKDIR /src
COPY --from=guest Cargo.toml Cargo.lock ./
COPY --from=guest .cargo/ ./.cargo/
COPY --from=guest appliance/ ./appliance/
COPY --from=tinyland / /tinyland/
ARG GUEST_ARCH=aarch64
ARG LLVM_URL=https://github.com/laputa-systems/llvm-prebuilt-musl/releases/download/llvm-musl-23.1.5-3afbb74/clang%2Bllvm-23.1.5-aarch64-linux-musl.tar.xz
ARG LLVM_SHA256=ca1c593163ac37d6656a5fddebec4c2714050c7345d9326e5afa8ea46949cede
RUN curl --proto '=https' --tlsv1.2 -sSfL "$LLVM_URL" -o /tmp/llvm.tar.xz \
 && echo "$LLVM_SHA256  /tmp/llvm.tar.xz" | sha256sum -c - \
 && mkdir /opt/llvm \
 && tar -xJf /tmp/llvm.tar.xz -C /opt/llvm --strip-components=1 \
 && rm /tmp/llvm.tar.xz
ENV CC_aarch64_unknown_linux_musl=/opt/llvm/bin/clang \
    CXX_aarch64_unknown_linux_musl=/opt/llvm/bin/clang++ \
    CFLAGS_aarch64_unknown_linux_musl=--target=aarch64-linux-musl \
    CXXFLAGS_aarch64_unknown_linux_musl=--target=aarch64-linux-musl \
    CC_x86_64_unknown_linux_musl=/opt/llvm/bin/clang \
    CXX_x86_64_unknown_linux_musl=/opt/llvm/bin/clang++ \
    CFLAGS_x86_64_unknown_linux_musl=--target=x86_64-linux-musl \
    CXXFLAGS_x86_64_unknown_linux_musl=--target=x86_64-linux-musl \
    RUSTFLAGS="-Clinker=/opt/llvm/bin/clang -Clink-self-contained=no -Clink-arg=--target=${GUEST_ARCH}-linux-musl -Clink-arg=-fuse-ld=mold"
RUN cargo build -Zbuild-std=std,panic_abort --locked --release --target ${GUEST_ARCH}-unknown-linux-musl --manifest-path /tinyland/Cargo.toml --target-dir /src/target -p tinyland -p seatd \
 && cargo build -Zbuild-std=std,panic_abort --locked --release --target ${GUEST_ARCH}-unknown-linux-musl -p bctl-guest

FROM guest-build AS xkbcommon-build
ARG GUEST_ARCH
RUN rev=$(sed -n 's|^source = "git+https://github.com/laputa-systems/kbvm?rev=\([0-9a-f]*\)#.*|\1|p' /tinyland/Cargo.lock | sort -u) \
 && [ "$(printf '%s\n' "$rev" | grep -c .)" = 1 ] \
 && git init -q /kbvm \
 && git -C /kbvm fetch -q --depth 1 https://github.com/laputa-systems/kbvm "$rev" \
 && git -C /kbvm checkout -q FETCH_HEAD \
 && cd /kbvm \
 && RUSTFLAGS="$RUSTFLAGS -Ctarget-feature=-crt-static" \
    cargo build -Zbuild-std=std,panic_abort -Zbuild-std-features=panic-unwind,llvm-libunwind \
        --locked --release --target ${GUEST_ARCH}-unknown-linux-musl -p kbvm-xkbcommon \
 && readelf -d target/${GUEST_ARCH}-unknown-linux-musl/release/libxkbcommon.so > /tmp/dynamic \
 && grep -q 'SONAME.*\[libxkbcommon.so.0\]' /tmp/dynamic \
 && [ "$(grep NEEDED /tmp/dynamic)" = "$(printf ' 0x0000000000000001 (NEEDED)             Shared library: [libc.musl-%s.so.1]' "$GUEST_ARCH")" ] \
 && cp target/${GUEST_ARCH}-unknown-linux-musl/release/libxkbcommon.so /libxkbcommon.so.0

FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS installer
ARG CHROMIUM_BUILD_REV=4275db88a4b3883c0316d99e03968a7f9ad9701a
RUN apk add --no-cache git \
 && git init -q /chromium-build \
 && git -C /chromium-build fetch -q --depth 1 https://github.com/laputa-systems/chromium-build "$CHROMIUM_BUILD_REV" \
 && git -C /chromium-build checkout -q FETCH_HEAD

FROM ${ROOTFS_BASE} AS rootfs
ARG GUEST_ARCH
RUN apk add --no-cache binutils python3
COPY --from=installer /chromium-build/scripts/install-runtime.py /chromium-build/scripts/runtime_install.py /chromium-build/scripts/linux_runtime.py /chromium-build/scripts/
COPY --from=browser-archive / /archive/
COPY --from=guest appliance/rootfs/ /rootfs/
COPY --from=guest-build /src/target/${GUEST_ARCH}-unknown-linux-musl/release/tinyland /rootfs/usr/local/bin/tinyland
COPY --from=guest-build /src/target/${GUEST_ARCH}-unknown-linux-musl/release/seatd /rootfs/usr/local/bin/seatd
COPY --from=guest-build /src/target/${GUEST_ARCH}-unknown-linux-musl/release/bctl-guest /rootfs/usr/local/bin/bctl-guest
COPY --from=xkbcommon-build /libxkbcommon.so.0 /xkbcommon/libxkbcommon.so.0
COPY stage-helium.sh qualify.py /stage/
RUN LLVM_READELF=readelf sh /stage/stage-helium.sh /archive /rootfs /chromium-build/scripts /xkbcommon/libxkbcommon.so.0 "$GUEST_ARCH" \
 && python3 -I /stage/qualify.py /rootfs "$GUEST_ARCH"

FROM scratch
COPY --from=rootfs /rootfs/ /
ENV PATH=/usr/local/bin
ENTRYPOINT ["/usr/local/bin/bctl-guest"]
CMD ["run"]
