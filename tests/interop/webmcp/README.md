# Qualify a WebMCP test image

Start with a built `target/debug/bctl`, `.work/images/helium.tar`, and the
runtime bundle for your host. The scripts support macOS ARM64 and Linux x86_64.
Use an isolated `BCTL_HOME` for each runtime.

## Prepare the HTTPS fixture

Generate a two-day fixture CA and server certificate under repo scratch:

```sh
sh tests/interop/webmcp/prepare-certificates.sh .work/tmp/webmcp-certs
```

Build the test image with that CA in the browser's NSS database:

```sh
sh tests/interop/webmcp/build-image.sh \
  .work/images/helium.tar .work/tmp/webmcp-certs/ca.crt \
  .work/images/helium-webmcp.tar
```

The base archive must use the OCI layout produced by the current image builder.
The script imports it under an isolated tag and keeps ordinary image tags intact.
Only the test image enables `WebMCPTesting` and `DevToolsWebMCPSupport`.
The wrapper preserves the browser's existing effective feature list.
TLS certificate verification stays enabled.

Run the fixture in another terminal:

```sh
python3 tests/interop/webmcp/serve.py \
  --cert .work/tmp/webmcp-certs/server.crt \
  --key .work/tmp/webmcp-certs/server.key
```

Keep the server running and use both printed ports below.
The first origin hosts the tools. The second hosts a cross-origin tool frame.

## Start SmolVM

```sh
export BCTL_HOME="$PWD/.work/tmp/webmcp-smolvm"
export BCTL_RUNTIME=smolvm
mkdir -p "$BCTL_HOME/images" "$BCTL_HOME/runtime"
ln -s "$PWD/.work/images/helium-webmcp.tar" "$BCTL_HOME/images/helium-webmcp.tar"
ln -s "$PWD/.work/runtime/smolvm" "$BCTL_HOME/runtime/smolvm"
target/debug/bctl up webmcp --image helium-webmcp --allow-private 100.96.0.0/30
target/debug/bctl tab new webmcp
IMAGE="$PWD/.work/images/helium-webmcp.tar"
```

## Start Nanos

Bootstrap the kernel toolchain first with `sh kernel/dev/bootstrap.sh`.
On macOS, set `HYDROGEN_KRUN` to the existing libkrun bundle before building:

```sh
sh tests/interop/webmcp/build-nanos-image.sh \
  .work/images/helium-webmcp.tar nanos-webmcp-qualified
export BCTL_HOME="$PWD/.work/tmp/webmcp-nanos"
export BCTL_RUNTIME=nanos
mkdir -p "$BCTL_HOME/images" "$BCTL_HOME/runtime/nanos"
for file in nanos-webmcp-qualified.img nanos-webmcp-qualified.profile.img nanos-webmcp-qualified.kernel.img; do
  ln -s "$PWD/.work/images/$file" "$BCTL_HOME/images/$file"
done
```

Populate the runtime directory, then start the session:

```sh
case $(uname -s) in
  Darwin) cp -R .work/runtime/nanos-krun/. "$BCTL_HOME/runtime/nanos/" ;;
  Linux)
    for tool in virtiofsd vhost-device-vsock; do
      ln -s "$PWD/.work/runtime/nanos/bin/$tool" "$BCTL_HOME/runtime/nanos/$tool"
    done ;;
esac
target/debug/bctl up webmcp --image nanos-webmcp-qualified --allow-private 100.96.0.0/30
target/debug/bctl tab new webmcp
IMAGE="$PWD/.work/images/nanos-webmcp-qualified.img"
```

## Run the checks

Set `PORT` and `CROSS_PORT` to the fixture's printed ports.
Set `IMAGE` to the test archive for SmolVM or the test `.img` for Nanos:

```sh
python3 tests/interop/webmcp/check.py \
  --bctl "$PWD/target/debug/bctl" --home "$BCTL_HOME" --session webmcp \
  --url "https://100.96.0.1:$PORT/?cross=https://100.96.0.1:$CROSS_PORT/frame" \
  --image "$IMAGE" --receipt .work/tmp/webmcp-receipt.json
```

Use agent-browser 0.43.0. The checker attaches through the browser capability
and explicitly selects the native target. Direct page connections do not
provide the session identities its WebMCP event collector requires.
The checker revokes the capability and closes its client after comparison.

Require a zero exit status and a `PASS` receipt.
The receipt records the image digest, browser command line, and passed checks.
Nanos refuses human takeover because it has no input devices.
Headed runtimes must refuse tool execution while a viewer holds control.

Stop the session and fixture server when finished:

```sh
target/debug/bctl down webmcp
```

Press Ctrl-C in the fixture server's terminal.

Keep test images isolated. They trust the fixture CA and enable experimental APIs.
Regenerate certificates and rebuild test images after the certificates expire.
