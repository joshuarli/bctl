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

FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS browser-archive
RUN apk add --no-cache chromium font-dejavu fontconfig binutils xz tar
COPY standin-archive.sh /usr/local/bin/standin-archive
RUN standin-archive /

FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS rootfs
RUN apk add --no-cache python3 xkeyboard-config xz tar
COPY --from=browser-archive /*.tar.xz /archive/
COPY stage-browser.sh /usr/local/bin/stage-browser
RUN stage-browser /archive /rootfs \
 && mkdir -p /rootfs/lib /rootfs/usr/share/X11 \
 && cp /lib/ld-musl-aarch64.so.1 /rootfs/lib/ \
 && ln -s ld-musl-aarch64.so.1 /rootfs/lib/libc.musl-aarch64.so.1 \
 && cp -a /usr/share/X11/xkb /rootfs/usr/share/X11/
COPY --from=guest appliance/rootfs/ /rootfs/
COPY --from=guest-build /src/target/aarch64-unknown-linux-musl/release/tinyland /rootfs/usr/local/bin/tinyland
COPY --from=guest-build /src/target/aarch64-unknown-linux-musl/release/seatd /rootfs/usr/local/bin/seatd
COPY --from=guest-build /src/target/aarch64-unknown-linux-musl/release/bctl-guest /rootfs/usr/local/bin/bctl-guest

FROM scratch
COPY --from=rootfs /rootfs/ /
ENV PATH=/usr/local/bin
ENTRYPOINT ["/usr/local/bin/bctl-guest"]
CMD ["run"]
