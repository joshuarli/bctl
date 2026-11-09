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
COPY --from=guest seatd-protocol/ ./seatd-protocol/
COPY --from=guest seatd/ ./seatd/
COPY --from=guest tinyland/ ./tinyland/
RUN cargo build --locked --release --target aarch64-unknown-linux-musl -p tinyland -p seatd -p bctl-guest

FROM guest-build AS xkbcommon-build
RUN rev=$(sed -n 's|^source = "git+https://github.com/laputa-systems/kbvm?rev=\([0-9a-f]*\)#.*|\1|p' Cargo.lock | sort -u) \
 && [ "$(printf '%s\n' "$rev" | grep -c .)" = 1 ] \
 && git init -q /kbvm \
 && git -C /kbvm fetch -q --depth 1 https://github.com/laputa-systems/kbvm "$rev" \
 && git -C /kbvm checkout -q FETCH_HEAD \
 && cd /kbvm \
 && RUSTFLAGS='-C target-feature=-crt-static' \
    cargo build --locked --release --target aarch64-unknown-linux-musl -p kbvm-xkbcommon \
 && readelf -d target/aarch64-unknown-linux-musl/release/libxkbcommon.so > /tmp/dynamic \
 && grep -q 'SONAME.*\[libxkbcommon.so.0\]' /tmp/dynamic \
 && [ "$(grep NEEDED /tmp/dynamic)" = "$(printf ' 0x0000000000000001 (NEEDED)             Shared library: [libc.musl-aarch64.so.1]')" ] \
 && cp target/aarch64-unknown-linux-musl/release/libxkbcommon.so /libxkbcommon.so.0

FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS installer
ARG CHROMIUM_BUILD_REV=c7856740687f3d3f47e9b1e596b0ef21e1c5a4d0
RUN apk add --no-cache git \
 && git init -q /chromium-build \
 && git -C /chromium-build fetch -q --depth 1 https://github.com/laputa-systems/chromium-build "$CHROMIUM_BUILD_REV" \
 && git -C /chromium-build checkout -q FETCH_HEAD

FROM alpine@sha256:e7a1a92a5bfeee40966aea60f0796b0e7917cc35591542701834f03a68fa3d18 AS rootfs
RUN apk add --no-cache binutils python3
COPY --from=installer /chromium-build/scripts/install-runtime.py /chromium-build/scripts/runtime_install.py /chromium-build/scripts/linux_runtime.py /chromium-build/scripts/
COPY --from=browser-archive / /archive/
COPY --from=guest appliance/rootfs/ /rootfs/
COPY --from=guest-build /src/target/aarch64-unknown-linux-musl/release/tinyland /rootfs/usr/local/bin/tinyland
COPY --from=guest-build /src/target/aarch64-unknown-linux-musl/release/seatd /rootfs/usr/local/bin/seatd
COPY --from=guest-build /src/target/aarch64-unknown-linux-musl/release/bctl-guest /rootfs/usr/local/bin/bctl-guest
COPY --from=xkbcommon-build /libxkbcommon.so.0 /xkbcommon/libxkbcommon.so.0
COPY stage-helium.sh qualify.py /stage/
RUN LLVM_READELF=readelf sh /stage/stage-helium.sh /archive /rootfs /chromium-build/scripts /xkbcommon/libxkbcommon.so.0 \
 && python3 -I /stage/qualify.py /rootfs

FROM scratch
COPY --from=rootfs /rootfs/ /
ENV PATH=/usr/local/bin
ENTRYPOINT ["/usr/local/bin/bctl-guest"]
CMD ["run"]
