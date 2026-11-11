#!/bin/sh
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
work=$root/.work/interop
browser_use_rev=070be98109c03f04a95f704533e8f6e9868d0525
agent_browser_rev=e7519957b9067c541825afa21a792fbe1f483047
export TMPDIR=$root/.work/tmp PYTHONDONTWRITEBYTECODE=1

fail() { echo "tests/interop/run.sh: $*" >&2; exit 1; }

case ${BCTL_RUNTIME:-} in
'') if [ "$(uname -s)" = Darwin ]; then runtime=smolvm; else runtime=nanos; fi ;;
smolvm | nanos) runtime=$BCTL_RUNTIME ;;
*) fail "BCTL_RUNTIME must be smolvm or nanos" ;;
esac
command -v uv >/dev/null || fail "uv is required"
command -v lsof >/dev/null || fail "lsof is required"
mkdir -p "$work" "$TMPDIR"

venv=$work/venv
if ! "$venv/bin/python" -c 'import browser_use, sys; sys.exit(browser_use.__version__ != "1.0.0")' 2>/dev/null; then
    uv venv -q --clear --python 3.12 "$venv" || fail "cannot create the browser-use virtualenv"
    VIRTUAL_ENV=$venv uv pip install -q "browser-use @ git+https://github.com/browser-use/browser-use@$browser_use_rev" ||
        fail "cannot install browser-use $browser_use_rev"
fi
agent_browser=$work/agent-browser/bin/agent-browser
if [ "$("$agent_browser" --version 2>/dev/null)" != "agent-browser 0.42.0" ]; then
    CARGO_TARGET_DIR=$work/agent-browser-target cargo install -q --locked --force \
        --git https://github.com/vercel-labs/agent-browser --rev "$agent_browser_rev" agent-browser --root "$work/agent-browser" ||
        fail "cannot build agent-browser $agent_browser_rev"
fi

cargo build -q -p bctl --manifest-path "$root/Cargo.toml" || fail "cannot build bctl"
bctl=$root/target/debug/bctl
home=$TMPDIR/interop-$$
mkdir -p "$home/runtime" "$home/images"
if [ "$runtime" = smolvm ]; then
    image=helium
    ln -s "$root/.work/runtime/smolvm" "$home/runtime/smolvm"
    ln -s "$root/.work/images/helium.tar" "$home/images/helium.tar"
    image_file=$root/.work/images/helium.tar
elif [ "$(uname -s)" = Darwin ]; then
    image=nanos-helium
    ln -s "$root/.work/runtime/nanos-krun" "$home/runtime/nanos"
    for f in nanos-helium.img nanos-helium.profile.img; do ln -s "$root/.work/images/$f" "$home/images/$f"; done
    image_file=$root/.work/images/nanos-helium.img
else
    image=nanos-helium
    mkdir -p "$home/runtime/nanos"
    ln -s "$root/.work/runtime/nanos/bin/vhost-device-vsock" "$home/runtime/nanos/vhost-device-vsock"
    for f in nanos-helium.img nanos-helium.kernel.img; do ln -s "$root/.work/images/$f" "$home/images/$f"; done
    image_file=$root/.work/images/nanos-helium.img
fi
[ -e "$image_file" ] || fail "missing $image_file: build the $runtime image first"
export BCTL BCTL_HOME BCTL_RUNTIME BCTL_IMAGE AGENT_BROWSER
BCTL=$bctl BCTL_HOME=$home BCTL_RUNTIME=$runtime BCTL_IMAGE=$image AGENT_BROWSER=$agent_browser
session=interop

fixture_pid=''
cleanup() {
    "$bctl" down "$session" >/dev/null 2>&1 || :
    daemon=$(lsof -t "$home/daemon.lock" 2>/dev/null || :)
    [ -z "$daemon" ] || kill "$daemon" 2>/dev/null || :
    [ -z "$fixture_pid" ] || kill "$fixture_pid" 2>/dev/null || :
    rm -rf "$home"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

"$venv/bin/python" "$root/tests/interop/fixture.py" >"$home/fixture.port" &
fixture_pid=$!
i=0
while [ ! -s "$home/fixture.port" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
fixture_port=$(cat "$home/fixture.port")
[ -n "$fixture_port" ] || fail "the fixture server did not start"

"$bctl" up "$session" --profile "$session" --image "$image" --allow-private 100.96.0.0/30 >/dev/null || fail "bctl up failed"
url_file=$home/cdp-url
(umask 077 && "$bctl" cdp-url "$session" >"$url_file") || fail "bctl cdp-url failed"

case ${BCTL_INTEROP_SUITE:-checks} in
checks | measure) suite=${BCTL_INTEROP_SUITE:-checks} ;;
*) fail "BCTL_INTEROP_SUITE must be checks or measure" ;;
esac
status=0
"$venv/bin/python" "$root/tests/interop/$suite.py" "$session" "$url_file" "$fixture_port" >"$home/checks.out" || status=$?
if [ "$suite" = measure ]; then
    cat "$home/checks.out"
    helium=$(curl -s "$(cat "$url_file")/json/version" | sed -n 's/.*"Browser": *"\([^"]*\)".*/\1/p')
else
    grep -v '^{' "$home/checks.out" || :
    helium=$(grep '^{' "$home/checks.out" | tail -n 1 | sed 's/.*"helium": "\([^"]*\)".*/\1/')
fi
dirty=$(git -C "$root" status --porcelain --untracked-files=no | head -c 1)
printf '\nreceipts\n'
printf '  bctl           %s%s\n' "$(git -C "$root" rev-parse HEAD)" "${dirty:+ (dirty)}"
printf '  host           %s %s\n' "$(uname -sm)" "$( (sw_vers -productVersion 2>/dev/null || uname -r) | head -n 1)"
if [ "$runtime" = smolvm ]; then
    runtime_receipt=$("$home/runtime/smolvm/smolvm" --version)
elif [ "$(uname -s)" = Darwin ]; then
    runtime_receipt="nanos on libkrun, hydrogen $(sed -n 's/^revision=//p' "$root/runtime/build-nanos-arm64.sh")"
else
    runtime_receipt="nanos on QEMU, hydrogen $(sed -n 's/^hydrogen_commit=//p' "$root/runtime/build-nanos.sh")"
fi
printf '  runtime        %s\n' "$runtime_receipt"
printf '  image          %s sha256:%s\n' "$image" "$(shasum -a 256 "$image_file" | cut -d' ' -f1)"
printf '  helium         %s\n' "${helium:-unknown}"
printf '  browser-use    1.0.0 %s, browser-harness %s\n' "$browser_use_rev" \
    "$("$venv/bin/python" -c 'import importlib.metadata as m; print(m.version("browser-harness"))')"
printf '  agent-browser  %s %s\n' "$("$agent_browser" --version | cut -d' ' -f2)" "$agent_browser_rev"
[ "$status" -eq 0 ] || fail "$suite failed"
printf 'interop %s: ok\n' "$suite"
