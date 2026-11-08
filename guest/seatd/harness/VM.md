# Run VM qualification

Prepare an x86-64 Linux host with KVM, QEMU, `apk`, a static C toolchain, `mkfs.ext4`, and Python 3. Build the musl seatd binary before running the guest. The guest uses virtual devices only.

Create a disposable guest root from the cached Tinyland Alpine root. These commands use the existing Alpine 3.20 package repositories and verify the guest packages with the Alpine keys in the copied root. They write only inside this repository.

```sh
exclude=$(git rev-parse --git-path info/exclude)
rg -qxF '.work/' "$exclude" || printf '\n.work/\n' >> "$exclude"
tinyland=$(cd ../tinyland && pwd)
mkdir -p .work/tmp .work/apk-cache .work/prepared-guest
export TMPDIR="$PWD/.work/tmp"
cp -a --reflink=auto $tinyland/.work/guest-root/. .work/prepared-guest/
apk --root "$PWD/.work/prepared-guest" \
  --keys-dir "$PWD/.work/prepared-guest/etc/apk/keys" \
  --repositories-file "$PWD/.work/prepared-guest/etc/apk/repositories" \
  --cache-dir "$PWD/.work/apk-cache" --arch x86_64 --usermode add cage eudev
```

Copy the cached Laputa compositor and libraries into the guest. The Alpine packages provide foot, eudev, libseat, and the remaining compositor dependencies. The Laputa archives supply dwl, wlroots, display information, libinput, and Wayland versions used by the proof.

```sh
packages="$(cd .. && pwd)/.out/mirror/packages/x86_64"
tar -xzf "$packages/dwl-minimal/dwl-minimal-0.9-1-6124ce719c95.tar.gz" \
  -C .work/prepared-guest usr/bin/dwl
tar -xzf "$packages/wlroots0.20/wlroots0.20-0.20.2-1-5a10430d3ebf.tar.gz" \
  -C .work/prepared-guest usr/lib/libwlroots-0.20.so
tar -xzf "$packages/libdisplay-info/libdisplay-info-0.4.0-1-7b1cdf26f532.tar.gz" \
  -C .work/prepared-guest usr/lib/libdisplay-info.so.0.4.0 usr/lib/libdisplay-info.so.4
for package in "$packages"/libinput/*.tar.gz \
  "$packages"/wayland-libs-client/*.tar.gz \
  "$packages"/wayland-libs-server/*.tar.gz; do
  tar -xzf "$package" -C .work/prepared-guest
done
```

Run each mode with the cached kernel and initramfs and the seatd binary to qualify.

```sh
for mode in transition shutdown compositor; do
  harness/vm-qualification.sh "$mode" .work/prepared-guest \
    $tinyland/.work/vmlinuz-virt \
    $tinyland/.work/initramfs-virt \
    target/x86_64-unknown-linux-musl/release/seatd
done
```

Inspect `.work/vm-qualification/<mode>/console.log` for the guest output. The compositor screenshot is `.work/vm-qualification/compositor/screen.ppm`. The `transition` mode checks release, acquire, device revocation and reopening, DRM master handoff, and restoration after closing the seat. The `shutdown` mode checks restoration after SIGTERM with a client connected. The `compositor` mode checks a rendered foot marker and keyboard input through dwl and the seatd backend.
