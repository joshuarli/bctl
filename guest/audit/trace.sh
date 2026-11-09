#!/bin/sh
# Run the B1 fixture (bctl-proof m1) on IMAGE_TAG with a /proc/*/maps sampler in the guest: a shell sampler when
# the image has /bin/sh, otherwise the one bctl-guest runs under BCTL_TRACE_MAPS=1.
# Usage: trace.sh IMAGE_TAG OUT.tsv   (build bctl-proof first; SMOLVM and PROOF override the paths)
set -eu
tag=${1:?usage: trace.sh IMAGE_TAG OUT.tsv}
out=${2:?usage: trace.sh IMAGE_TAG OUT.tsv}
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$here/../.." && pwd)
main_worktree=$(git -C "$repo" worktree list --porcelain | sed -n '1s/^worktree //p')
smolvm=${SMOLVM:-$main_worktree/.work/runtime/smolvm/smolvm}
proof=${PROOF:-$repo/.work/target/debug/bctl-proof}
work=$repo/.work/tmp
trace_tag=r0-trace-$$
build=$work/r0-trace-build-$$
image=$work/$trace_tag.tar
[ -x "$smolvm" ] || { echo "trace.sh: no SmolVM at $smolvm" >&2; exit 1; }
[ -x "$proof" ] || { echo "trace.sh: no bctl-proof at $proof" >&2; exit 1; }

mkdir -p "$build"
if docker run --rm --platform linux/arm64 --entrypoint /bin/sh "$tag" -c true > /dev/null 2>&1; then
    cp "$here/trace-sampler.sh" "$build/r0-trace-sampler"
    cp "$here/trace-entry.sh" "$build/r0-trace-entry"
    cat > "$build/Dockerfile" <<DOCKER
FROM $tag
COPY --chmod=755 r0-trace-sampler r0-trace-entry /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/r0-trace-entry"]
CMD ["run"]
DOCKER
    read_trace="cat /run/r0-maps.tsv"
else
    cat > "$build/Dockerfile" <<DOCKER
FROM $tag
ENV BCTL_TRACE_MAPS=1
DOCKER
    read_trace="bctl-guest maps"
fi
docker build -q --platform linux/arm64 -t "$trace_tag" "$build" > /dev/null
docker save "$trace_tag" -o "$image"
docker rmi -f "$trace_tag" > /dev/null
rm -rf "$build"

BCTL_PROOF_IMAGE=$image "$proof" m1 &
pid=$!
machine=bctl-proof-$pid-1
: > "$out.part"
while kill -0 "$pid" 2>/dev/null; do
    # shellcheck disable=SC2086
    if "$smolvm" machine exec --name "$machine" -- $read_trace > "$out.next" 2>/dev/null && [ -s "$out.next" ]; then
        mv "$out.next" "$out.part"
    fi
    sleep 1
done
status=0
wait "$pid" || status=$?
rm -f "$out.next" "$image"
[ "$status" = 0 ] || { echo "trace.sh: bctl-proof m1 failed ($status)" >&2; rm -f "$out.part"; exit "$status"; }
[ -s "$out.part" ] || { echo "trace.sh: sampler produced no data" >&2; rm -f "$out.part"; exit 1; }
mv "$out.part" "$out"
