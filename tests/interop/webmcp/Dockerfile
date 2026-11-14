ARG BASE_IMAGE
FROM ${BASE_IMAGE} AS base
FROM alpine:3.22@sha256:49340b346798875b5592e5d420241f2f9cfba5f47e62ebb1446a1b34734e898f AS builder
RUN apk add --no-cache build-base nss-tools
COPY browser.c /browser.c
RUN gcc -static -O2 -Wall -Wextra -Werror /browser.c -o /browser
COPY --from=base / /rootfs/
COPY ca.crt /ca.crt
RUN mv /rootfs/opt/browser/helium /rootfs/opt/browser/helium.real \
 && cp /browser /rootfs/opt/browser/helium \
 && mkdir -p /rootfs/home/browser/.pki/nssdb \
 && certutil -N --empty-password -d sql:/rootfs/home/browser/.pki/nssdb \
 && certutil -A -d sql:/rootfs/home/browser/.pki/nssdb -n bctl-webmcp-fixture -t 'C,,' -i /ca.crt \
 && chown -R 1000:27 /rootfs/home/browser/.pki
FROM scratch
COPY --from=builder /rootfs/ /
ENV PATH=/usr/local/bin
ENTRYPOINT ["/usr/local/bin/bctl-guest"]
CMD ["run"]
