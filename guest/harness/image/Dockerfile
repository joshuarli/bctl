FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS buildenv

RUN apk add --no-cache \
        bash build-base ca-certificates curl git mold musl-dev pkgconf \
        libseat-dev xkeyboard-config

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
 && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain none \
 && rm /tmp/rustup-init.sh \
 && rustup toolchain install nightly-2026-09-15 --profile minimal \
        --component rust-src --component clippy

FROM buildenv AS tinyland-build

WORKDIR /src/tinyland
COPY tinyland/ ./
RUN cargo build --locked --release --target aarch64-unknown-linux-musl

FROM buildenv AS tests

WORKDIR /src
COPY tinyland/ ./tinyland/
COPY seatd/ ./seatd/
RUN cargo fetch --locked --manifest-path /src/tinyland/Cargo.toml \
 && cargo fetch --locked --manifest-path /src/seatd/Cargo.toml

FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS image

RUN apk add --no-cache busybox font-dejavu foot libgcc libseat seatd xkeyboard-config \
 && adduser -D -u 1000 -G seat -s /bin/sh tiny

COPY --from=tinyland-build /src/tinyland/target/aarch64-unknown-linux-musl/release/tinyland /usr/local/bin/tinyland
COPY tinyland/guest/test-app /usr/local/bin/guest-test
COPY harness/image/tinyland-session /usr/local/bin/tinyland-session

RUN chmod 755 /usr/local/bin/tinyland /usr/local/bin/guest-test /usr/local/bin/tinyland-session

ENTRYPOINT ["/usr/local/bin/tinyland-session"]
