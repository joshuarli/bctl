FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS libudev
RUN apk add --no-cache gcc make musl-dev linux-headers git \
 && git clone -q https://github.com/laputa-systems/libudev-zero /src \
 && git -C /src checkout -q 8f1d3d0624496f1c4967f18d6afaddbd552d8bce \
 && make -C /src libudev.so.1

FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk add --no-cache chromium sway seatd libudev-zero font-dejavu xkeyboard-config su-exec socat \
 && adduser -D -u 1000 -G video browser
COPY --from=libudev /src/libudev.so.1 /usr/lib/libudev.so.1
COPY bctl-guest /usr/local/bin/bctl-guest
ENTRYPOINT ["/usr/local/bin/bctl-guest"]
CMD ["run"]
