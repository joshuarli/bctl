# Userland census

The census attributes every file in an image rootfs to one of three classes
from the pure-Rust userland rule in `docs/CAMPAIGN-ROADMAP.md` (Decisions):

- **rust**: Rust-owned, with the crate and source named.
- **exception**: a named exception (musl, Mesa, ALSA, Mozilla NSS, and the
  browser with what it statically bundles) with its own library closure.
- **gap**: everything else. A gap is a file the claim "pure-Rust userland"
  does not yet cover.

A file the attribution table does not cover fails the run. Gaps do not fail
it unless `--deny-gaps` is given (exit 3), which is the A9 check.

## Stand-in image: read this first

Every number below is for `bctl/ref-alpine-chromium` (`images/ref-alpine-chromium`,
Docker image `32936ffc6734`, Alpine 3.22 aarch64, Alpine `chromium` 142.0.7444.59
and the Sway stack). It is test scaffolding, not the product. Helium is not
built yet. The `browser` exception here is the Alpine `chromium` APK standing
in for Helium; the Alpine APK links the system libraries Helium bundles or
ships, so this image has far more gaps than the Helium image will. Rerun on
the Helium image and replace this section; nothing in `guest/audit` is
specific to Alpine except the optional apk-database attribution.

R1 added a second stand-in, `bctl/ref-scratch-chromium`
(`images/ref-scratch-chromium`): no base image, busybox, or shell; the Rust
`bctl-guest`, `guest/tinyland`, `guest/seatd`, musl's loader, XKB data, and
the same Alpine `chromium` 142.0.7444.59 packed with its runtime closure in
the Helium archive layout under `/opt/browser`. Its results are in
**Results on ref-scratch-chromium**. When the Helium archive exists, the same
image built around it (`docs/image-contract.md`) replaces both stand-ins.

## Running it

```sh
cargo build -p bctl-proof
guest/audit/trace.sh bctl/ref-alpine-chromium .work/tmp/trace.tsv
CHROMIUM_BUILD_SCRIPTS=../chromium-build/scripts guest/audit/audit.sh bctl/ref-alpine-chromium --trace .work/tmp/trace.tsv --json .work/tmp/report.json
```

The same two commands take `bctl/ref-scratch-chromium` (build it first with
`images/build.sh ref-scratch-chromium`).

`audit.sh` exports the image rootfs into a throwaway Docker volume and runs
`census.py` in an Alpine container with `python3` and `binutils` (GNU
`readelf`; set nothing, the container supplies it). `census.py` can also run on
any extracted rootfs directory: `census.py ROOT --chromium-build-scripts DIR
--readelf readelf`.

The tool is Python rather than a Rust crate because chromium-build's
`scripts/linux_runtime.py` already owns ELF parsing (`readelf -h -l -d`, the
glibc and musl library sets, interpreter checks) and the roadmap requires
reusing it. `census.py` imports it from the directory named by `--chromium-build-scripts`
(`audit.sh` takes it from `CHROMIUM_BUILD_SCRIPTS`) without modifying it, and
uses `is_elf`, `inspect_elf`, `readelf_output`, and its musl/glibc constants.
It does not call `audit_tree`, because that follows symlinks on the host and
rejects the absolute symlinks an image rootfs legitimately contains.

## Method

- **Walk.** Every regular file and symlink in the rootfs, without following
  symlinks. Bytes count each inode once. `exclude` in the table drops
  container-export artifacts (`/.dockerenv`, `/dev`, `/proc`, `/sys`, and the
  `/etc/hostname`, `/etc/hosts`, `/etc/resolv.conf`, and `/etc/mtab` that
  `docker export` writes into every container, an image from scratch
  included).
- **Attribution.** `guest/audit/attribution.toml` holds ordered rules, first
  match wins. A rule matches `paths` (glob over the absolute path) or
  `packages` (the owning apk package from `/lib/apk/db/installed`; a symlink
  with no owner takes its target's owner, which covers busybox applets). A
  `rust` rule must name its `source`; every rule names a `reason`. Gap rules
  group packages by why they are gaps; the census still reports each package
  as its own unit. A rule with `elf = true` (or `false`) matches only ELF (or
  only non-ELF) files, which separates the Rust `bctl-guest` from the shell
  script at the same path in the Alpine images. A rule with `unit = "file"`
  reports each file as its own unit, which keeps the browser's library
  closure itemized in a rootfs with no package database.
- **ELF.** For each ELF file: DT_NEEDED, interpreter, SONAME, and kind
  (`dynamic` executable with an interpreter, `library`, `static`, `loader`).
  Findings flag glibc dependencies, NEEDED libraries absent from the image,
  and non-musl interpreters.
- **Consumers.** For a unit: the units whose ELF files list its SONAME in
  DT_NEEDED, the apk packages that depend on it, and the executables that
  mapped its files in the trace. `none` means nothing links it: it is an
  entry point that `bctl-guest` starts, or a data package no one declares.
- **Runtime loads.** `trace.sh` builds a derived image whose entrypoint also
  runs a shell sampler (`trace-sampler.sh`), then runs `bctl-proof m1` (the
  B1 fixture: typing, select popup, click, wheel, popup window, navigation)
  unchanged on it. The sampler reads `/proc/*/maps` every 0.3 s inside the
  guest and accumulates `executable<TAB>mapped file` pairs; the host copies
  the accumulated file out once a second until the run ends. A library that
  an executable maps but that is not in its static DT_NEEDED closure was
  `dlopen`ed, by the executable or by a library it loaded.
  `/proc/*/maps` was chosen over `strace -f -e openat` because the image has
  no strace and adding it would change the image under test; `LD_DEBUG` is
  glibc-only. Limits: a mapping that comes and goes between samples is
  missed, failed `dlopen` attempts are invisible, and the snapshot stops at
  the last copy before the VM stops.
  An image without `/bin/sh` gets a different tap: `trace.sh` derives an
  image that only sets `BCTL_TRACE_MAPS=1`, the Rust `bctl-guest run` then
  samples the same pairs every 0.3 s into `/run/bctl/maps.tsv`, and the host
  reads them with `smolvm machine exec -- bctl-guest maps`. The VM that `bctl-proof` creates is named
  `bctl-proof-<pid>-1`, not `r0-`, because the runner chooses it.

## Results on ref-alpine-chromium

| class | units | files | bytes |
|---|---:|---:|---:|
| rust | 0 | 0 | 0 B |
| exception | 19 | 266 | 508.4 MiB |
| gap | 207 | 2830 | 208.8 MiB |
| total | 226 | 3096 | 717.2 MiB |

### Rust-owned

None.

### Named exceptions

| unit | rule | bytes | files | ELF | basis |
|---|---|---:|---:|---:|---|
| chromium | browser | 247.1 MiB | 31 | 6 | stand-in for Helium and what it statically bundles (Alpine chromium APK) |
| llvm20-libs | mesa | 153.1 MiB | 4 | 1 | named exception with its library closure (owner 2026-11-08) |
| mesa | mesa | 92.3 MiB | 10 | 8 | named exception with its library closure (owner 2026-11-08) |
| spirv-tools | mesa | 5.2 MiB | 17 | 16 | named exception with its library closure (owner 2026-11-08) |
| nss | nss | 3.6 MiB | 20 | 10 | Mozilla Network Security Services, named exception |
| sqlite-libs | nss | 1.5 MiB | 2 | 1 | Mozilla Network Security Services, named exception |
| hwdata-pci | mesa | 1.4 MiB | 1 | 0 | named exception with its library closure (owner 2026-11-08) |
| alsa-lib | alsa | 1.3 MiB | 83 | 3 | named exception (use_alsa=true) |
| libdrm | mesa | 869.5 KiB | 21 | 10 | named exception with its library closure (owner 2026-11-08) |
| musl | musl | 706.5 KiB | 2 | 1 | retained by laputa LAP-51 |
| nspr | nss | 391.8 KiB | 3 | 3 | Mozilla Network Security Services, named exception |
| mesa-egl | mesa | 323.1 KiB | 2 | 1 | named exception with its library closure (owner 2026-11-08) |
| mesa-gles | mesa | 131.5 KiB | 4 | 2 | named exception with its library closure (owner 2026-11-08) |
| libelf | mesa | 130.0 KiB | 2 | 1 | named exception with its library closure (owner 2026-11-08) |
| mesa-dri-gallium | mesa | 129.8 KiB | 56 | 1 | named exception with its library closure (owner 2026-11-08) |
| libpciaccess | mesa | 65.7 KiB | 2 | 1 | named exception with its library closure (owner 2026-11-08) |
| mesa-gbm | mesa | 65.7 KiB | 2 | 1 | named exception with its library closure (owner 2026-11-08) |
| libxshmfence | mesa | 65.7 KiB | 2 | 1 | named exception with its library closure (owner 2026-11-08) |
| wayland-libs-egl | mesa | 65.6 KiB | 2 | 1 | named exception with its library closure (owner 2026-11-08) |

### Gaps

| unit | bytes | files | ELF | in trace | consumers | why it is a gap |
|---|---:|---:|---:|---|---|---|
| icu-data-full | 30.4 MiB | 1 | 0 | no | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| ffmpeg-libavcodec | 13.1 MiB | 2 | 1 | yes | chromium, ffmpeg-libavformat | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| gtk+3.0 | 10.4 MiB | 37 | 22 | yes | chromium | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libmagic | 10.0 MiB | 3 | 1 | no | file | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| font-dejavu | 9.8 MiB | 38 | 0 | yes | chromium, sway | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| pipewire-libs | 7.7 MiB | 59 | 56 | yes | chromium | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| x265-libs | 6.4 MiB | 1 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| aom-libs | 6.4 MiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| glib | 5.7 MiB | 29 | 16 | yes | at-spi2-core, at-spi2-core-libs, cairo-gobject, chromium, gdk-pixbuf, gobject-introspection (+11) | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| sway-wallpapers | 4.8 MiB | 8 | 0 | no | none | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libcrypto3 | 4.8 MiB | 10 | 6 | yes | apk-tools, chromium, ffmpeg-libavformat, ffmpeg-libavutil, libapk2, libsrt (+4) | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| libSvtAv1Enc | 4.1 MiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| xkeyboard-config | 3.1 MiB | 292 | 0 | no | libxkbcommon | C libxkbcommon; R3 replaces it with a kbvm-backed library |
| font-opensans | 2.9 MiB | 20 | 0 | no | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| glib-and-system-libs | 2.7 MiB | 924 | 0 | yes | chromium | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libstdc++ | 2.6 MiB | 2 | 1 | yes | abseil-cpp-raw-logging-internal, abseil-cpp-strings, abseil-cpp-strings-internal, chromium, crc32c, double-conversion (+23) | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| ffmpeg-libavformat | 2.5 MiB | 2 | 1 | yes | chromium | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libcamera | 2.5 MiB | 20 | 5 | no | libcamera-ipa, pipewire-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| shared-mime-info | 2.4 MiB | 5 | 1 | no | gdk-pixbuf, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libvpx | 2.4 MiB | 3 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libx11 | 2.3 MiB | 195 | 2 | yes | at-spi2-core, at-spi2-core-libs, cairo, chromium, ffmpeg-libavutil, gtk+3.0 (+22) | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libjxl | 2.1 MiB | 8 | 4 | yes | chromium, ffmpeg-libavcodec | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| gnutls | 2.0 MiB | 2 | 1 | yes | chromium, cups-libs, libcamera | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| libxcb | 2.0 MiB | 50 | 25 | yes | cairo, chromium, libpulse, libva, libx11, mesa (+5) | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libcamera-ipa | 1.9 MiB | 10 | 5 | no | libcamera | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libunistring | 1.9 MiB | 2 | 1 | yes | chromium, gnutls, libidn2 | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| p11-kit | 1.8 MiB | 6 | 3 | yes | chromium, gnutls | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| x264-libs | 1.7 MiB | 1 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libpulse | 1.5 MiB | 8 | 5 | yes | chromium | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| rav1e-libs | 1.4 MiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| openh264 | 1.4 MiB | 2 | 1 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| roc-toolkit-libs | 1.4 MiB | 2 | 1 | no | pipewire-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| harfbuzz | 1.2 MiB | 3 | 1 | yes | chromium, gtk+3.0, harfbuzz-subset, pango, sway | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libopenmpt | 1.2 MiB | 2 | 1 | yes | chromium, ffmpeg-libavformat | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| gdk-pixbuf | 1.2 MiB | 20 | 16 | yes | chromium, gtk+3.0, gtk-update-icon-cache, swaybar | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| cairo | 1.1 MiB | 5 | 3 | yes | cairo-gobject, chromium, gtk+3.0, pango, sway, swaybar (+1) | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libxml2 | 1.1 MiB | 2 | 1 | yes | chromium, ffmpeg-libavformat, libxkbcommon, libxslt, llvm20-libs, shared-mime-info (+1) | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libepoxy | 1.1 MiB | 2 | 1 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libsndfile | 1.1 MiB | 12 | 11 | yes | chromium, libpulse, pipewire-libs | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| openexr-libopenexr | 1.1 MiB | 2 | 1 | no | libjxl | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| mbedtls | 1.0 MiB | 6 | 3 | yes | chromium, librist | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| fontconfig | 987.3 KiB | 76 | 10 | yes | cairo, chromium, font-dejavu, font-opensans, gtk+3.0, libxft (+2) | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| wlroots | 965.2 KiB | 1 | 1 | yes | sway | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| webrtc-audio-processing-2 | 962.1 KiB | 1 | 1 | no | pipewire-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| harfbuzz-subset | 961.9 KiB | 2 | 1 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| busybox | 906.2 KiB | 310 | 1 | yes | busybox-binsh | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| brotli-libs | 901.6 KiB | 6 | 3 | yes | chromium, freetype, libjxl, sway | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libsrt | 898.8 KiB | 2 | 1 | yes | chromium, ffmpeg-libavformat | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libssl3 | 847.9 KiB | 1 | 1 | yes | chromium, ffmpeg-libavformat, libapk2, socat, ssl_client | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| libvorbis | 837.2 KiB | 6 | 3 | yes | chromium, ffmpeg-libavcodec, libopenmpt, libsndfile | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| openexr-libopenexrcore | 835.4 KiB | 2 | 1 | no | openexr-libopenexr | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| ffmpeg-libavutil | 834.0 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec, ffmpeg-libavformat, ffmpeg-libswresample | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| pcre2 | 771.7 KiB | 4 | 2 | yes | chromium, glib, sway | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libzmq | 770.0 KiB | 2 | 1 | yes | chromium, ffmpeg-libavformat | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libdav1d | 769.7 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| pango | 744.5 KiB | 14 | 4 | yes | chromium, gtk+3.0, sway, swaybar, swaynag | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| freetype | 706.1 KiB | 2 | 1 | yes | cairo, chromium, fontconfig, harfbuzz, libxft, mkfontscale (+2) | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| socat | 648.0 KiB | 4 | 3 | yes | none | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| nettle | 643.9 KiB | 4 | 2 | yes | chromium, gnutls | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| cups-libs | 643.4 KiB | 2 | 2 | yes | chromium, gtk+3.0 | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| zstd-libs | 641.8 KiB | 2 | 1 | yes | chromium, libelf, llvm20-libs, mesa, sway, tiff | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| encodings | 630.7 KiB | 51 | 0 | no | font-dejavu | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| orc | 591.0 KiB | 2 | 1 | no | libpulse | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| sway | 587.8 KiB | 4 | 2 | yes | none | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libtheora | 581.8 KiB | 6 | 3 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libelogind | 580.8 KiB | 2 | 1 | yes | libseat, sway | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| openexr-libiex | 578.0 KiB | 2 | 1 | no | openexr-libilmthread, openexr-libopenexr | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| pixman | 577.9 KiB | 2 | 1 | yes | cairo, chromium, sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| vulkan-loader | 577.9 KiB | 2 | 1 | yes | chromium, sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libbluray | 519.0 KiB | 5 | 4 | yes | chromium, ffmpeg-libavformat | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| basu | 518.5 KiB | 2 | 2 | no | swaybar | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libjpeg-turbo | 514.0 KiB | 2 | 1 | yes | chromium, gdk-pixbuf, libjxl, tiff | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libpisp | 460.5 KiB | 3 | 1 | no | libcamera, libcamera-ipa | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libssh | 455.1 KiB | 2 | 1 | yes | chromium, ffmpeg-libavformat | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| libxslt | 453.8 KiB | 5 | 3 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| tiff | 450.0 KiB | 2 | 1 | no | gdk-pixbuf | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| gmp | 449.8 KiB | 2 | 1 | yes | chromium, gnutls, nettle | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| libxt | 399.1 KiB | 2 | 1 | no | libxmu | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| lcms2 | 396.7 KiB | 2 | 1 | yes | chromium, libjxl, sway, wlroots | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libva | 391.5 KiB | 8 | 4 | yes | chromium, ffmpeg-libavcodec, ffmpeg-libavutil | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| mpg123-libs | 390.7 KiB | 6 | 3 | yes | chromium, libopenmpt, libsndfile | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libxkbcommon | 387.8 KiB | 4 | 2 | yes | chromium, gtk+3.0, sway, wlroots | C libxkbcommon; R3 replaces it with a kbvm-backed library |
| libncursesw | 386.9 KiB | 2 | 1 | yes | readline, socat | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| xvidcore | 386.6 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| opus | 385.7 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec, libsndfile, pipewire-libs | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| readline | 344.4 KiB | 3 | 1 | yes | socat | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| libunwind | 327.5 KiB | 10 | 5 | no | libcamera, roc-toolkit-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libatk-bridge-2.0 | 327.4 KiB | 4 | 2 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libmount | 322.6 KiB | 2 | 1 | yes | chromium, glib, sway | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libgomp | 322.5 KiB | 2 | 1 | yes | chromium, soxr | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| dbus-libs | 322.0 KiB | 2 | 1 | yes | at-spi2-core, at-spi2-core-libs, avahi-libs, chromium, libatk-bridge-2.0, libpulse (+1) | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libwebp | 321.9 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec, libwebpdemux, libwebpmux, tiff | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libflac | 321.8 KiB | 2 | 1 | yes | chromium, libsndfile | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| imath | 321.7 KiB | 2 | 1 | no | openexr-libopenexr | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libinput-libs | 313.1 KiB | 52 | 1 | yes | sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| gobject-introspection | 309.5 KiB | 16 | 1 | no | gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| xdg-utils | 289.2 KiB | 8 | 0 | no | chromium | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| libatk-1.0 | 267.9 KiB | 3 | 1 | yes | chromium, gtk+3.0, libatk-bridge-2.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| alpine-base | 265.9 KiB | 12 | 0 | no | none | Alpine base system and package manager; R1 removes the base |
| libsodium | 259.1 KiB | 2 | 1 | yes | chromium, libzmq | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libuv | 258.8 KiB | 3 | 1 | no | roc-toolkit-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| at-spi2-core-libs | 258.2 KiB | 2 | 1 | yes | at-spi2-core, chromium, libatk-bridge-2.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| simdutf | 258.0 KiB | 2 | 1 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| lame-libs | 257.8 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec, libsndfile | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| xz-libs | 257.7 KiB | 2 | 1 | yes | chromium, libunwind, libxml2, sway | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| at-spi2-core | 257.3 KiB | 8 | 2 | no | none | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| pkgconf | 213.5 KiB | 6 | 3 | no | hicolor-icon-theme, shared-mime-info | Alpine base system and package manager; R1 removes the base |
| musl-utils | 197.8 KiB | 5 | 3 | no | none | Alpine base system and package manager; R1 removes the base |
| libxmu | 196.6 KiB | 4 | 2 | no | xset | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| fribidi | 196.4 KiB | 3 | 2 | yes | chromium, gtk+3.0, pango, sway | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| soxr | 196.0 KiB | 4 | 2 | yes | chromium, ffmpeg-libswresample, libpulse | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libapk2 | 195.6 KiB | 1 | 1 | no | apk-tools | Alpine base system and package manager; R1 removes the base |
| libblkid | 194.8 KiB | 2 | 1 | yes | chromium, libmount, sway | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libidn2 | 194.0 KiB | 2 | 1 | yes | chromium, gnutls | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| libpng | 194.0 KiB | 2 | 1 | yes | cairo, chromium, freetype, gdk-pixbuf, libjxl, sway | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| librist | 193.8 KiB | 2 | 1 | yes | chromium, ffmpeg-libavformat | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libintl | 193.8 KiB | 2 | 1 | yes | at-spi2-core-libs, avahi-libs, chromium, gdk-pixbuf, glib, gtk+3.0 (+5) | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libexpat | 193.7 KiB | 2 | 1 | yes | chromium, fontconfig, mesa, mesa-egl, mesa-gbm, sway | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libdisplay-info | 193.7 KiB | 2 | 1 | yes | sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| ca-certificates-bundle | 174.6 KiB | 4 | 0 | no | apk-tools, libapk2 | Alpine base system and package manager; R1 removes the base |
| chromium-system-libs | 146.9 KiB | 10 | 0 | no | none | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libeconf | 140.0 KiB | 3 | 2 | yes | chromium, libblkid, sway | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| avahi-libs | 132.2 KiB | 4 | 2 | yes | chromium, cups-libs, pipewire-libs | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| libcap2 | 131.6 KiB | 4 | 2 | yes | libelogind, sway | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libvdpau | 131.4 KiB | 6 | 2 | yes | chromium, ffmpeg-libavutil | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| xcb-util-wm | 131.3 KiB | 4 | 2 | yes | sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libxext | 130.9 KiB | 2 | 1 | yes | cairo, chromium, gtk+3.0, libva, libvdpau, libxi (+6) | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| swaybar | 130.5 KiB | 1 | 1 | no | none | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libice | 130.5 KiB | 2 | 1 | no | libsm, libxt | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| ffmpeg-libswresample | 130.0 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libxft | 130.0 KiB | 2 | 1 | no | pango | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libevdev | 129.9 KiB | 2 | 1 | yes | libinput-libs, sway | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| abseil-cpp-strings | 129.9 KiB | 1 | 1 | no | webrtc-audio-processing-2 | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| double-conversion | 129.9 KiB | 2 | 1 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| tdb-libs | 129.9 KiB | 2 | 1 | no | libpulse | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libgcc | 129.9 KiB | 1 | 1 | yes | abseil-cpp-raw-logging-internal, abseil-cpp-strings, abseil-cpp-strings-internal, chromium, double-conversion, libcamera (+19) | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| zlib | 129.9 KiB | 2 | 1 | yes | apk-tools, cairo, chromium, cups-libs, ffmpeg-libavcodec, ffmpeg-libavformat (+16) | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| yaml | 129.8 KiB | 2 | 1 | no | libcamera | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| graphite2 | 129.7 KiB | 2 | 1 | yes | chromium, harfbuzz, sway | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libdeflate | 129.7 KiB | 1 | 1 | no | openexr-libopenexrcore | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| lilv-libs | 129.7 KiB | 2 | 1 | no | pipewire-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| ncurses-terminfo-base | 98.7 KiB | 41 | 0 | no | libncursesw | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| wayland-libs-cursor | 78.2 KiB | 2 | 1 | yes | chromium, gtk+3.0, swaybar, swaynag | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libudev-zero | 74.8 KiB | 1 | 1 | yes | chromium, libcamera, libinput-libs, pipewire-libs, sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libfontenc | 69.1 KiB | 2 | 1 | no | mkfontscale | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| libbz2 | 68.7 KiB | 2 | 1 | yes | chromium, ffmpeg-libavformat, freetype, sway | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| xprop | 68.0 KiB | 1 | 1 | no | xdg-utils | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| apk-tools | 67.6 KiB | 1 | 1 | no | none | Alpine base system and package manager; R1 removes the base |
| libbsd | 66.9 KiB | 2 | 1 | yes | chromium, libxdmcp, sway | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| wayland-libs-server | 66.7 KiB | 2 | 1 | yes | chromium, mesa, mesa-egl, sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| wayland-libs-client | 66.7 KiB | 2 | 1 | yes | chromium, gtk+3.0, libva, mesa-egl, sway, swaybar (+3) | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| swaynag | 66.4 KiB | 1 | 1 | no | none | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| mkfontscale | 66.3 KiB | 2 | 1 | no | font-dejavu, font-opensans | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| file | 66.2 KiB | 1 | 1 | no | xdg-utils | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| numactl | 66.2 KiB | 2 | 1 | yes | chromium, x265-libs | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| gtk-update-icon-cache | 66.1 KiB | 2 | 1 | no | gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| json-c | 66.1 KiB | 2 | 1 | yes | sway, swaybar | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| openexr-libilmthread | 66.0 KiB | 2 | 1 | no | openexr-libopenexr | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| abseil-cpp-strings-internal | 66.0 KiB | 1 | 1 | no | abseil-cpp-strings | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libffi | 66.0 KiB | 2 | 1 | yes | chromium, glib, gobject-introspection, llvm20-libs, p11-kit, sway (+2) | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libuuid | 66.0 KiB | 2 | 1 | no | libsm | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| abseil-cpp-raw-logging-internal | 66.0 KiB | 1 | 1 | no | abseil-cpp-strings, abseil-cpp-strings-internal | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libmd | 66.0 KiB | 2 | 1 | yes | chromium, libbsd, sway | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libseat | 65.9 KiB | 1 | 1 | yes | sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libhwy | 65.9 KiB | 2 | 1 | yes | chromium, libjxl | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libxrender | 65.9 KiB | 2 | 1 | yes | cairo, chromium, libxcursor, libxft, libxrandr, pango (+1) | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libtasn1 | 65.9 KiB | 2 | 1 | yes | chromium, gnutls, p11-kit | TLS and crypto libraries pulled in by ffmpeg, cups, and avahi |
| ssl_client | 65.9 KiB | 1 | 1 | no | none | Alpine base system and package manager; R1 removes the base |
| libxtst | 65.9 KiB | 2 | 1 | no | at-spi2-core | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| scanelf | 65.8 KiB | 1 | 1 | no | musl-utils | Alpine base system and package manager; R1 removes the base |
| libsm | 65.8 KiB | 2 | 1 | no | libxt | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| seatd | 65.8 KiB | 1 | 1 | yes | none | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| libxi | 65.8 KiB | 2 | 1 | yes | at-spi2-core-libs, chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libxdmcp | 65.8 KiB | 2 | 1 | yes | chromium, libxcb, sway | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| zix-libs | 65.8 KiB | 2 | 1 | no | lilv-libs, sord-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| cairo-gobject | 65.8 KiB | 2 | 1 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libxinerama | 65.8 KiB | 2 | 1 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libxrandr | 65.8 KiB | 2 | 1 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libltdl | 65.8 KiB | 3 | 1 | no | libpulse | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libwebpdemux | 65.7 KiB | 2 | 1 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| speexdsp | 65.7 KiB | 2 | 1 | no | libpulse, roc-toolkit-libs | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| su-exec | 65.7 KiB | 1 | 1 | no | none | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| xset | 65.7 KiB | 1 | 1 | no | xdg-utils | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |
| cjson | 65.7 KiB | 2 | 1 | yes | chromium, librist | glib and the basic C libraries beneath it; R2 reports what survives in Helium |
| libsharpyuv | 65.7 KiB | 2 | 1 | yes | chromium, libwebp | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| xcb-util-renderutil | 65.7 KiB | 2 | 1 | yes | sway, wlroots | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| crc32c | 65.7 KiB | 2 | 1 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| giflib | 65.7 KiB | 2 | 1 | no | libjxl | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libasyncns | 65.7 KiB | 2 | 1 | yes | chromium, libpulse | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libogg | 65.7 KiB | 2 | 1 | yes | chromium, libflac, libsndfile, libtheora, libvorbis | codec and container libraries the Alpine chromium APK links through system ffmpeg |
| libwebpmux | 65.7 KiB | 2 | 1 | yes | chromium, ffmpeg-libavcodec | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| libxau | 65.7 KiB | 2 | 1 | yes | chromium, libxcb, sway | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libxcomposite | 65.7 KiB | 2 | 1 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libxcursor | 65.7 KiB | 2 | 1 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| libxdamage | 65.7 KiB | 2 | 1 | yes | chromium, gtk+3.0 | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| minizip | 65.7 KiB | 2 | 1 | yes | chromium | libraries the Alpine chromium APK links from the system; Helium bundles its own copies or the library is a real gap |
| mtdev | 65.7 KiB | 2 | 1 | yes | libinput-libs, sway | C compositor, seat, and input stack; replaced by tinyland, guest/seatd, and kbvm |
| serd-libs | 65.7 KiB | 2 | 1 | no | lilv-libs, sord-libs, sratom | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| sord-libs | 65.7 KiB | 2 | 1 | no | lilv-libs, sratom | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| sratom | 65.7 KiB | 2 | 1 | no | lilv-libs | PipeWire, PulseAudio and libcamera stack the Alpine chromium APK links for audio capture |
| libxfixes | 65.6 KiB | 2 | 1 | yes | chromium, gtk+3.0, libva, libxcursor | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| hicolor-icon-theme | 54.3 KiB | 2 | 0 | no | gtk-update-icon-cache | GTK3 and its closure; owner removed GTK from Helium builds (R2) |
| alpine-baselayout-data | 18.6 KiB | 15 | 0 | no | alpine-baselayout | Alpine base system and package manager; R1 removes the base |
| alpine-keys | 14.2 KiB | 43 | 0 | no | alpine-release | Alpine base system and package manager; R1 removes the base |
| alpine-baselayout | 6.3 KiB | 13 | 0 | no | none | Alpine base system and package manager; R1 removes the base |
| alpine-release | 343 B | 5 | 0 | no | none | Alpine base system and package manager; R1 removes the base |
| busybox-binsh | 0 B | 1 | 0 | no | alpine-baselayout, fontconfig, gdk-pixbuf, glib, gtk+3.0, gtk-update-icon-cache (+4) | busybox and shell tooling; the diagnostic image keeps them, the product image drops them |

### ELF files

478 ELF files: 89 dynamic, 388 library, 1 loader.

Static ELF executables (no PT_INTERP):

- none

ELF audit findings (glibc dependencies, missing libraries, foreign interpreters):

- none

### Runtime loads from the traced B1 run

Libraries mapped by a process but outside its static DT_NEEDED closure, which means a `dlopen` by it or by a library it loaded. Processes with none are omitted.

`/usr/bin/sway`:

| library | unit | class |
|---|---|---|
| `/usr/lib/libvulkan.so.1.4.313` | vulkan-loader | gap |

`/usr/lib/chromium/chromium`:

| library | unit | class |
|---|---|---|
| `/usr/lib/gbm/dri_gbm.so` | mesa | exception |
| `/usr/lib/libLLVM.so.20.1` | llvm20-libs | exception |
| `/usr/lib/libSPIRV-Tools.so` | spirv-tools | exception |
| `/usr/lib/libXcursor.so.1.0.2` | libxcursor | gap |
| `/usr/lib/libXinerama.so.1.0.0` | libxinerama | gap |
| `/usr/lib/libcairo-gobject.so.2.11804.4` | cairo-gobject | gap |
| `/usr/lib/libdrm_amdgpu.so.1.124.0` | libdrm | exception |
| `/usr/lib/libelf-0.193.so` | libelf | exception |
| `/usr/lib/libepoxy.so.0.0.0` | libepoxy | gap |
| `/usr/lib/libgallium-25.1.9.so` | mesa | exception |
| `/usr/lib/libgdk-3.so.0.2418.32` | gtk+3.0 | gap |
| `/usr/lib/libgdk_pixbuf-2.0.so.0.4200.12` | gdk-pixbuf | gap |
| `/usr/lib/libgtk-3.so.0.2418.32` | gtk+3.0 | gap |
| `/usr/lib/libpangocairo-1.0.so.0.5600.3` | pango | gap |
| `/usr/lib/libpangoft2-1.0.so.0.5600.3` | pango | gap |
| `/usr/lib/libvulkan.so.1.4.313` | vulkan-loader | gap |
| `/usr/lib/libwayland-client.so.0.23.1` | wayland-libs-client | gap |
| `/usr/lib/libwayland-cursor.so.0.23.1` | wayland-libs-cursor | gap |
| `/usr/lib/libwayland-egl.so.1.23.1` | wayland-libs-egl | exception |
| `/usr/lib/libwayland-server.so.0.23.1` | wayland-libs-server | gap |
| `/usr/lib/libxcb-present.so.0.0.0` | libxcb | gap |
| `/usr/lib/libxcb-randr.so.0.1.0` | libxcb | gap |
| `/usr/lib/libxcb-sync.so.1.0.0` | libxcb | gap |
| `/usr/lib/libxcb-xfixes.so.0.0.0` | libxcb | gap |
| `/usr/lib/libxshmfence.so.1.0.0` | libxshmfence | exception |

Data files mapped by `exe`, by unit:

- `/usr/bin/sway`: created at run time 6, font-dejavu 1
- `/usr/lib/chromium/chromium`: chromium 5, created at run time 156, font-dejavu 1, glib-and-system-libs 1

## Results on ref-scratch-chromium

Docker image `44a8a768f73b`, traced with the `bctl-guest` tap during one B1
run.

| class | units | files | bytes |
|---|---:|---:|---:|
| rust | 4 | 5 | 6.1 MiB |
| exception | 5 | 92 | 450.1 MiB |
| gap | 143 | 434 | 101.9 MiB |
| total | 152 | 531 | 558.1 MiB |

### Rust-owned

| unit | rule | bytes | files | ELF | basis |
|---|---|---:|---:|---:|---|
| tinyland | tinyland | 4.6 MiB | 1 | 1 | guest/tinyland (crate tinyland) |
| bctl-guest | bctl-guest | 881.6 KiB | 1 | 1 | guest/appliance (crate bctl-guest) |
| seatd | seatd | 658.5 KiB | 1 | 1 | guest/seatd (crate seatd) |
| bctl-guest-accounts | bctl-guest-accounts | 117 B | 2 | 0 | guest/appliance/rootfs/etc |

### Named exceptions

| unit | rule | bytes | files | ELF | basis |
|---|---|---:|---:|---:|---|
| browser-archive | browser-archive | 256.9 MiB | 68 | 6 | the browser archive with its fonts and fontconfig; here Alpine chromium packed in Helium's layout (images/ref-scratch-chromium) |
| mesa | mesa | 186.6 MiB | 10 | 10 | named exception with its library closure (owner 2026-11-08) |
| nss | nss | 4.9 MiB | 11 | 11 | Mozilla Network Security Services, named exception |
| alsa | alsa | 1.0 MiB | 1 | 1 | named exception (use_alsa=true) |
| musl | musl | 706.5 KiB | 2 | 1 | retained by laputa LAP-51 |

### Gaps

| unit | bytes | files | ELF | in trace | consumers | why it is a gap |
|---|---:|---:|---:|---|---|---|
| libavcodec.so.60 | 13.1 MiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libgtk-3.so.0 | 7.1 MiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libx265.so.209 | 6.4 MiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libaom.so.3 | 6.4 MiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libcrypto.so.3 | 4.4 MiB | 1 | 1 | yes | browser-archive, libavformat.so.60, libavutil.so.58, libsrt.so.1.5, libssh.so.4, libssl.so.3 | a shared library the browser archive ships and loads at run time that no exception names |
| libSvtAv1Enc.so.2 | 4.1 MiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| xkb-data | 3.1 MiB | 292 | 0 | no | none | xkeyboard-config data that tinyland's kbvm compiles the US keymap from at start |
| libstdc++.so.6 | 2.6 MiB | 1 | 1 | yes | browser-archive, libcrc32c.so.1, libdouble-conversion.so.3, libhwy.so.1, libjxl.so.0.10, libjxl_cms.so.0.10 (+8) | a shared library the browser archive ships and loads at run time that no exception names |
| libavformat.so.60 | 2.5 MiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libvpx.so.9 | 2.4 MiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libgnutls.so.30 | 2.0 MiB | 1 | 1 | yes | browser-archive, libcups.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libgio-2.0.so.0 | 2.0 MiB | 1 | 1 | yes | browser-archive, libatspi.so.0, libgdk-3.so.0, libgdk_pixbuf-2.0.so.0, libgtk-3.so.0, libpango-1.0.so.0 (+1) | a shared library the browser archive ships and loads at run time that no exception names |
| libunistring.so.5 | 1.9 MiB | 1 | 1 | yes | browser-archive, libgnutls.so.30, libidn2.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libjxl.so.0.10 | 1.8 MiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libx264.so.164 | 1.7 MiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libp11-kit.so.0 | 1.5 MiB | 1 | 1 | yes | browser-archive, libgnutls.so.30 | a shared library the browser archive ships and loads at run time that no exception names |
| librav1e.so.0.7 | 1.4 MiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libglib-2.0.so.0 | 1.4 MiB | 1 | 1 | yes | browser-archive, libatk-1.0.so.0, libatk-bridge-2.0.so.0, libatspi.so.0, libcairo-gobject.so.2, libgdk-3.so.0 (+9) | a shared library the browser archive ships and loads at run time that no exception names |
| libopenh264.so.8 | 1.4 MiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libopenmpt.so.0 | 1.2 MiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libX11.so.6 | 1.1 MiB | 1 | 1 | yes | browser-archive, libXcomposite.so.1, libXcursor.so.1, libXdamage.so.1, libXext.so.6, libXfixes.so.3 (+11) | a shared library the browser archive ships and loads at run time that no exception names |
| libxml2.so.2 | 1.1 MiB | 1 | 1 | yes | browser-archive, libavformat.so.60, libxslt.so.1, mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libepoxy.so.0 | 1.1 MiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libharfbuzz.so.0 | 1.1 MiB | 1 | 1 | yes | browser-archive, libgtk-3.so.0, libharfbuzz-subset.so.0, libpango-1.0.so.0, libpangocairo-1.0.so.0, libpangoft2-1.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libcairo.so.2 | 966.5 KiB | 1 | 1 | yes | browser-archive, libcairo-gobject.so.2, libgdk-3.so.0, libgtk-3.so.0, libpangocairo-1.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libgdk-3.so.0 | 966.2 KiB | 1 | 1 | yes | browser-archive, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libharfbuzz-subset.so.0 | 961.9 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libsrt.so.1.5 | 898.8 KiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libssl.so.3 | 847.9 KiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libavutil.so.58 | 834.0 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libavformat.so.60, libswresample.so.4 | a shared library the browser archive ships and loads at run time that no exception names |
| libpipewire-0.3.so.0 | 770.6 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libzmq.so.5 | 770.0 KiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libdav1d.so.7 | 769.7 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libfreetype.so.6 | 706.1 KiB | 1 | 1 | yes | browser-archive, libcairo.so.2, libfontconfig.so.1, libharfbuzz.so.0, libpangoft2-1.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libpcre2-8.so.0 | 706.0 KiB | 1 | 1 | yes | browser-archive, libglib-2.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libbrotlienc.so.1 | 642.2 KiB | 1 | 1 | yes | browser-archive, libjxl.so.0.10 | a shared library the browser archive ships and loads at run time that no exception names |
| libzstd.so.1 | 641.8 KiB | 1 | 1 | yes | browser-archive, mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libmbedcrypto.so.16 | 578.1 KiB | 1 | 1 | yes | browser-archive, librist.so.4 | a shared library the browser archive ships and loads at run time that no exception names |
| libpixman-1.so.0 | 577.9 KiB | 1 | 1 | yes | browser-archive, libcairo.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libcups.so.2 | 577.9 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libvorbisenc.so.2 | 577.8 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libsndfile.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libpulsecommon-17.0.so | 514.0 KiB | 1 | 1 | yes | browser-archive, libpulse.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libjpeg.so.8 | 514.0 KiB | 1 | 1 | yes | browser-archive, libgdk_pixbuf-2.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libssh.so.4 | 455.1 KiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libgobject-2.0.so.0 | 451.8 KiB | 1 | 1 | yes | browser-archive, libatk-1.0.so.0, libatk-bridge-2.0.so.0, libatspi.so.0, libcairo-gobject.so.2, libgdk-3.so.0 (+6) | a shared library the browser archive ships and loads at run time that no exception names |
| libsndfile.so.1 | 451.1 KiB | 1 | 1 | yes | browser-archive, libpulsecommon-17.0.so | a shared library the browser archive ships and loads at run time that no exception names |
| libgmp.so.10 | 449.8 KiB | 1 | 1 | yes | browser-archive, libgnutls.so.30, libhogweed.so.6 | a shared library the browser archive ships and loads at run time that no exception names |
| liblcms2.so.2 | 396.7 KiB | 1 | 1 | yes | browser-archive, libjxl_cms.so.0.10 | a shared library the browser archive ships and loads at run time that no exception names |
| libpango-1.0.so.0 | 387.2 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0, libgtk-3.so.0, libpangocairo-1.0.so.0, libpangoft2-1.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libxvidcore.so.4 | 386.6 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libopus.so.0 | 385.7 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libsndfile.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libpulse.so.0 | 322.9 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libmount.so.1 | 322.6 KiB | 1 | 1 | yes | browser-archive, libgio-2.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libgomp.so.1 | 322.5 KiB | 1 | 1 | yes | browser-archive, libsoxr.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libdbus-1.so.3 | 322.0 KiB | 1 | 1 | yes | browser-archive, libatk-bridge-2.0.so.0, libatspi.so.0, libavahi-client.so.3, libpulse.so.0, libpulsecommon-17.0.so | a shared library the browser archive ships and loads at run time that no exception names |
| libhogweed.so.6 | 322.0 KiB | 1 | 1 | yes | browser-archive, libgnutls.so.30 | a shared library the browser archive ships and loads at run time that no exception names |
| libnettle.so.8 | 322.0 KiB | 1 | 1 | yes | browser-archive, libgnutls.so.30, libhogweed.so.6 | a shared library the browser archive ships and loads at run time that no exception names |
| libwebp.so.7 | 321.9 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libwebpdemux.so.2, libwebpmux.so.3 | a shared library the browser archive ships and loads at run time that no exception names |
| libxkbcommon | 321.9 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0 | C libxkbcommon; R3 replaces it with a kbvm-backed library |
| libbluray.so.2 | 321.8 KiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libFLAC.so.12 | 321.8 KiB | 1 | 1 | yes | browser-archive, libsndfile.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libatk-bridge-2.0.so.0 | 261.5 KiB | 1 | 1 | yes | browser-archive, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libmpg123.so.0 | 259.2 KiB | 1 | 1 | yes | browser-archive, libopenmpt.so.0, libsndfile.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libsodium.so.26 | 259.1 KiB | 1 | 1 | yes | browser-archive, libzmq.so.5 | a shared library the browser archive ships and loads at run time that no exception names |
| libatspi.so.0 | 258.2 KiB | 1 | 1 | yes | browser-archive, libatk-bridge-2.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libxslt.so.1 | 258.0 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libsimdutf.so.24 | 258.0 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libmp3lame.so.0 | 257.8 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libsndfile.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libfontconfig.so.1 | 257.8 KiB | 1 | 1 | yes | browser-archive, libcairo.so.2, libgdk-3.so.0, libgtk-3.so.0, libpangocairo-1.0.so.0, libpangoft2-1.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| liblzma.so.5 | 257.7 KiB | 1 | 1 | yes | browser-archive, libxml2.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libblkid.so.1 | 194.8 KiB | 1 | 1 | yes | browser-archive, libmount.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libgdk_pixbuf-2.0.so.0 | 194.0 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libidn2.so.0 | 194.0 KiB | 1 | 1 | yes | browser-archive, libgnutls.so.30 | a shared library the browser archive ships and loads at run time that no exception names |
| libpng16.so.16 | 194.0 KiB | 1 | 1 | yes | browser-archive, libcairo.so.2, libfreetype.so.6, libgdk_pixbuf-2.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libtheoraenc.so.1 | 194.0 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libatk-1.0.so.0 | 193.8 KiB | 1 | 1 | yes | browser-archive, libatk-bridge-2.0.so.0, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| librist.so.4 | 193.8 KiB | 1 | 1 | yes | browser-archive, libavformat.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libva.so.2 | 193.8 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libavutil.so.58, libva-drm.so.2, libva-x11.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb.so.1 | 193.8 KiB | 1 | 1 | yes | browser-archive, libX11.so.6, libcairo.so.2, libpulsecommon-17.0.so, libva-x11.so.2, libxcb-dri3.so.0 (+7) | a shared library the browser archive ships and loads at run time that no exception names |
| libintl.so.8 | 193.8 KiB | 1 | 1 | yes | browser-archive, libatk-1.0.so.0, libatspi.so.0, libavahi-common.so.3, libgdk-3.so.0, libgdk_pixbuf-2.0.so.0 (+6) | a shared library the browser archive ships and loads at run time that no exception names |
| libbrotlicommon.so.1 | 193.7 KiB | 1 | 1 | yes | browser-archive, libbrotlidec.so.1, libbrotlienc.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libexpat.so.1 | 193.7 KiB | 1 | 1 | yes | browser-archive, libfontconfig.so.1, mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libvorbis.so.0 | 193.7 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libopenmpt.so.0, libsndfile.so.1, libvorbisenc.so.2, libvorbisfile.so.3 | a shared library the browser archive ships and loads at run time that no exception names |
| libXext.so.6 | 130.9 KiB | 1 | 1 | yes | browser-archive, libXi.so.6, libXinerama.so.1, libXrandr.so.2, libcairo.so.2, libgdk-3.so.0 (+2) | a shared library the browser archive ships and loads at run time that no exception names |
| libsoxr.so.0 | 130.2 KiB | 1 | 1 | yes | browser-archive, libswresample.so.4 | a shared library the browser archive ships and loads at run time that no exception names |
| libswresample.so.4 | 130.0 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libdouble-conversion.so.3 | 129.9 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libgcc_s.so.1 | 129.9 KiB | 1 | 1 | yes | browser-archive, libdouble-conversion.so.3, libopenh264.so.8, libopenmpt.so.0, librav1e.so.0.7, libsimdutf.so.24 (+4) | a shared library the browser archive ships and loads at run time that no exception names |
| libtheoradec.so.1 | 129.9 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libtheoraenc.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libz.so.1 | 129.9 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60, libavformat.so.60, libcairo.so.2, libcups.so.2, libfreetype.so.6 (+8) | a shared library the browser archive ships and loads at run time that no exception names |
| libfribidi.so.0 | 129.8 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0, libgtk-3.so.0, libpango-1.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libgraphite2.so.3 | 129.7 KiB | 1 | 1 | yes | browser-archive, libharfbuzz.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libpangoft2-1.0.so.0 | 129.7 KiB | 1 | 1 | yes | browser-archive, libgtk-3.so.0, libpangocairo-1.0.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libwayland-cursor.so.0 | 78.2 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libbz2.so.1 | 68.7 KiB | 1 | 1 | yes | browser-archive, libavformat.so.60, libfreetype.so.6 | a shared library the browser archive ships and loads at run time that no exception names |
| libbsd.so.0 | 66.9 KiB | 1 | 1 | yes | browser-archive, libXdmcp.so.6 | a shared library the browser archive ships and loads at run time that no exception names |
| libwayland-server.so.0 | 66.7 KiB | 1 | 1 | no | mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libwayland-client.so.0 | 66.7 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0, libwayland-cursor.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libnuma.so.1 | 66.2 KiB | 1 | 1 | yes | browser-archive, libx265.so.209 | a shared library the browser archive ships and loads at run time that no exception names |
| libavahi-client.so.3 | 66.1 KiB | 1 | 1 | yes | browser-archive, libcups.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libavahi-common.so.3 | 66.1 KiB | 1 | 1 | yes | browser-archive, libavahi-client.so.3, libcups.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libffi.so.8 | 66.0 KiB | 1 | 1 | yes | browser-archive, libgobject-2.0.so.0, libp11-kit.so.0, libwayland-client.so.0, libwayland-server.so.0, mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libmd.so.0 | 66.0 KiB | 1 | 1 | yes | browser-archive, libbsd.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libXrender.so.1 | 65.9 KiB | 1 | 1 | yes | browser-archive, libXcursor.so.1, libXrandr.so.2, libcairo.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libhwy.so.1 | 65.9 KiB | 1 | 1 | yes | browser-archive, libjxl.so.0.10 | a shared library the browser archive ships and loads at run time that no exception names |
| libudev.so.1 | 65.9 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libtasn1.so.6 | 65.9 KiB | 1 | 1 | yes | browser-archive, libgnutls.so.30 | a shared library the browser archive ships and loads at run time that no exception names |
| libeconf.so.0 | 65.9 KiB | 1 | 1 | yes | browser-archive, libblkid.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libva-x11.so.2 | 65.8 KiB | 1 | 1 | yes | browser-archive, libavutil.so.58 | a shared library the browser archive ships and loads at run time that no exception names |
| libXi.so.6 | 65.8 KiB | 1 | 1 | yes | browser-archive, libatspi.so.0, libgdk-3.so.0, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libXdmcp.so.6 | 65.8 KiB | 1 | 1 | yes | browser-archive, libxcb.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libpangocairo-1.0.so.0 | 65.8 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libXinerama.so.1 | 65.8 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libXrandr.so.2 | 65.8 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libcairo-gobject.so.2 | 65.8 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb-dri3.so.0 | 65.8 KiB | 1 | 1 | yes | browser-archive, libva-x11.so.2, mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb-present.so.0 | 65.8 KiB | 1 | 1 | no | mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb-randr.so.0 | 65.8 KiB | 1 | 1 | no | mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb-render.so.0 | 65.8 KiB | 1 | 1 | yes | browser-archive, libcairo.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb-shm.so.0 | 65.8 KiB | 1 | 1 | yes | browser-archive, libcairo.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb-sync.so.1 | 65.8 KiB | 1 | 1 | no | mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libxcb-xfixes.so.0 | 65.8 KiB | 1 | 1 | no | mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libgmodule-2.0.so.0 | 65.8 KiB | 1 | 1 | yes | browser-archive, libgdk_pixbuf-2.0.so.0, libgio-2.0.so.0, libgtk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libva-drm.so.2 | 65.7 KiB | 1 | 1 | yes | browser-archive, libavutil.so.58 | a shared library the browser archive ships and loads at run time that no exception names |
| libwebpdemux.so.2 | 65.7 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libvorbisfile.so.3 | 65.7 KiB | 1 | 1 | yes | browser-archive, libopenmpt.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libcjson.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive, librist.so.4 | a shared library the browser archive ships and loads at run time that no exception names |
| libsharpyuv.so.0 | 65.7 KiB | 1 | 1 | yes | browser-archive, libwebp.so.7 | a shared library the browser archive ships and loads at run time that no exception names |
| libvdpau.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive, libavutil.so.58 | a shared library the browser archive ships and loads at run time that no exception names |
| libXau.so.6 | 65.7 KiB | 1 | 1 | yes | browser-archive, libxcb.so.1 | a shared library the browser archive ships and loads at run time that no exception names |
| libXcomposite.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libXcursor.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libXdamage.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive, libgdk-3.so.0 | a shared library the browser archive ships and loads at run time that no exception names |
| libasyncns.so.0 | 65.7 KiB | 1 | 1 | yes | browser-archive, libpulsecommon-17.0.so | a shared library the browser archive ships and loads at run time that no exception names |
| libbrotlidec.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive, libfreetype.so.6, libjxl.so.0.10 | a shared library the browser archive ships and loads at run time that no exception names |
| libcrc32c.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libminizip.so.1 | 65.7 KiB | 1 | 1 | yes | browser-archive | a shared library the browser archive ships and loads at run time that no exception names |
| libogg.so.0 | 65.7 KiB | 1 | 1 | yes | browser-archive, libFLAC.so.12, libsndfile.so.1, libtheoraenc.so.1, libvorbis.so.0, libvorbisfile.so.3 | a shared library the browser archive ships and loads at run time that no exception names |
| libwebpmux.so.3 | 65.7 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |
| libXfixes.so.3 | 65.6 KiB | 1 | 1 | yes | browser-archive, libXcursor.so.1, libgdk-3.so.0, libgtk-3.so.0, libva-x11.so.2 | a shared library the browser archive ships and loads at run time that no exception names |
| libX11-xcb.so.1 | 65.6 KiB | 1 | 1 | yes | browser-archive, libva-x11.so.2, mesa | a shared library the browser archive ships and loads at run time that no exception names |
| libjxl_cms.so.0.10 | 63.4 KiB | 1 | 1 | yes | browser-archive, libjxl.so.0.10 | a shared library the browser archive ships and loads at run time that no exception names |
| libjxl_threads.so.0.10 | 13.7 KiB | 1 | 1 | yes | browser-archive, libavcodec.so.60 | a shared library the browser archive ships and loads at run time that no exception names |

### ELF files

174 ELF files: 3 dynamic, 167 library, 1 loader, 3 static.

Static ELF executables (no PT_INTERP):

- `/usr/local/bin/bctl-guest` (bctl-guest, rust)
- `/usr/local/bin/seatd` (seatd, rust)
- `/usr/local/bin/tinyland` (tinyland, rust)

ELF audit findings (glibc dependencies, missing libraries, foreign interpreters):

- none

### Runtime loads from the traced B1 run

Libraries mapped by a process but outside its static DT_NEEDED closure, which means a `dlopen` by it or by a library it loaded. Processes with none are omitted.

`/opt/browser/chromium`:

| library | unit | class |
|---|---|---|
| `/opt/browser/lib/libXcursor.so.1` | libXcursor.so.1 | gap |
| `/opt/browser/lib/libXinerama.so.1` | libXinerama.so.1 | gap |
| `/opt/browser/lib/libcairo-gobject.so.2` | libcairo-gobject.so.2 | gap |
| `/opt/browser/lib/libepoxy.so.0` | libepoxy.so.0 | gap |
| `/opt/browser/lib/libgdk-3.so.0` | libgdk-3.so.0 | gap |
| `/opt/browser/lib/libgdk_pixbuf-2.0.so.0` | libgdk_pixbuf-2.0.so.0 | gap |
| `/opt/browser/lib/libgtk-3.so.0` | libgtk-3.so.0 | gap |
| `/opt/browser/lib/libpangocairo-1.0.so.0` | libpangocairo-1.0.so.0 | gap |
| `/opt/browser/lib/libpangoft2-1.0.so.0` | libpangoft2-1.0.so.0 | gap |
| `/opt/browser/lib/libwayland-client.so.0` | libwayland-client.so.0 | gap |
| `/opt/browser/lib/libwayland-cursor.so.0` | libwayland-cursor.so.0 | gap |
| `/opt/browser/lib/libwayland-egl.so.1` | mesa | exception |

Data files mapped by `exe`, by unit:

- `/opt/browser/chromium`: browser-archive 6, created at run time 154

## Reading the scratch results

- Every gap but one is inside the browser's runtime closure: the 141
  libraries under `/opt/browser/lib` that no exception names (98.5 MiB) and
  `libxkbcommon.so.0` (R3), which the browser and GTK link. The one gap
  outside it is `xkb-data`, the xkeyboard-config tree under
  `/usr/share/X11/xkb` (3.1 MiB): no ELF links it, because tinyland's kbvm
  reads it at start to compile the US keymap it hands to clients. Removing
  it needs a tinyland that carries its keymap, or an owner decision that the
  data is an exception.
- Rust-owned: `tinyland`, `seatd`, `bctl-guest` (all three static, no
  PT_INTERP), and the two account files. The only C outside the browser
  archive is musl's loader.
- The closure is the DT_NEEDED closure of every ELF file in the browser
  directory plus the libraries the browser mapped outside its DT_NEEDED
  closure in a traced B1 run on `ref-alpine-tinyland`, plus the four NSS
  modules chromium-build also seeds. Five of the 141 were not mapped in this
  run (`libwayland-server`, `libxcb-present`, `libxcb-randr`, `libxcb-sync`,
  `libxcb-xfixes`); they belong to Mesa's GBM and DRI3 paths.
- Mesa (186.6 MiB, mostly `libLLVM`) ships but stays unloaded: GBM looks for
  its backend only at its built-in `/usr/lib/gbm`, so `dri_gbm.so` in the
  archive's flat `lib/` is never found, and `libgallium`, `libLLVM`, and
  `libSPIRV-Tools` are never mapped. The GPU process exits during
  initialization and Chromium composites in software, as it does on
  `ref-alpine-tinyland` (no native `libEGL.so.1` there either). A Helium
  archive built by `copy_system_closure` has the same layout, so P2 has to
  decide how Helium's Mesa finds its GBM backend.
- The NSS modules (`libsoftokn3`, `libfreeblpriv3`, `libfreebl3`,
  `libnssckbi`) were not mapped: the B1 fixture is served over plain HTTP.

## Reading the stand-in results

- No file is Rust-owned in `ref-alpine-chromium`. That image contains C
  `seatd`, `libseat`, `libudev-zero` (laputa's C fork), Sway, wlroots, and a
  shell `bctl-guest`. The table's `rust` rules cover `guest/tinyland`,
  `guest/seatd`, and the Rust `bctl-guest`; `ref-alpine-tinyland` reports
  its tinyland and seatd under them.
- Most gap bytes belong to the browser's system-library closure: codecs
  through ffmpeg, the PipeWire/PulseAudio stack, TLS libraries, GTK3, and
  glib. The Helium archive bundles or drops these, so they say little about
  the Helium result; R2 owns what survives there.
- The trace shows the Alpine browser loading GTK3 (`libgtk-3`, `libgdk-3`,
  `libpangocairo`, `libepoxy`), Mesa (`libgallium`, `libLLVM`, `libSPIRV-Tools`,
  `dri_gbm.so`, `libdrm_amdgpu`), X11 and XCB pieces (`libXcursor`,
  `libXinerama`, `libxcb-*`), and `libwayland-client/-cursor/-egl/-server` at
  run time, none of them in the browser's static closure. `libxkbcommon` is in
  the browser's static closure (and Sway's, wlroots', and GDK's).
- Gaps that nothing in the trace mapped (the `in trace` column) were not used
  by this fixture run. That is a candidate list for R1, not proof that
  nothing needs them.
- Mesa's closure is attributed by hand to the `mesa` rule:
  `llvm20-libs`, `spirv-tools`, `libdrm`, `libelf`, `libpciaccess`,
  `libxshmfence`, `hwdata-pci` have only Mesa or Mesa's own libraries as
  consumers in this image (`libdrm` also has other consumers; it stays under
  Mesa because `libgbm` needs it). `sqlite-libs` is in the NSS closure for the
  same reason.
