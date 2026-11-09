FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f

ARG WLCS_COMMIT=2b385328366de5db19521b9b3eef81861430d984

RUN apk add --no-cache bash boost-dev build-base ca-certificates cmake curl git \
        gmock gtest-dev mold musl-dev pkgconf samurai wayland-dev wayland-protocols

RUN git init /wlcs-src \
 && git -C /wlcs-src fetch --depth 1 https://github.com/canonical/wlcs "$WLCS_COMMIT" \
 && git -C /wlcs-src checkout --detach FETCH_HEAD \
 && cmake -G Ninja -S /wlcs-src -B /wlcs-build -DCMAKE_BUILD_TYPE=Debug \
 && cmake --build /wlcs-build --target wlcs

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    RUSTFLAGS="-Ctarget-feature=-crt-static"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
 && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain nightly-2026-09-15 \
 && rm /tmp/rustup-init.sh

WORKDIR /src
