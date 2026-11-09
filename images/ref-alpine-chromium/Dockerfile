FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f
RUN apk add --no-cache chromium sway seatd libudev-zero font-dejavu xkeyboard-config su-exec socat \
 && adduser -D -u 1000 -G video browser
COPY bctl-guest /usr/local/bin/bctl-guest
ENTRYPOINT ["/usr/local/bin/bctl-guest"]
CMD ["run"]
