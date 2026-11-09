#!/bin/sh
# Census a Docker image tag: export its rootfs to a volume and run census.py under Alpine with readelf.
# Usage: audit.sh IMAGE_TAG [--trace TRACE.tsv] [--json REPORT.json] [--deny-gaps]
set -eu
tag=${1:?usage: audit.sh IMAGE_TAG [census.py options]}
shift
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
scripts=${CHROMIUM_BUILD_SCRIPTS:?set CHROMIUM_BUILD_SCRIPTS to a chromium-build scripts directory}
volume=r0-rootfs-$$
container=r0-export-$$
trap 'docker rm -f "$container" > /dev/null 2>&1 || true; docker volume rm -f "$volume" > /dev/null 2>&1 || true' EXIT

docker volume create "$volume" > /dev/null
docker create --name "$container" "$tag" > /dev/null
docker export "$container" | docker run --rm -i -v "$volume":/rootfs alpine:3.22 tar -x -C /rootfs

mounts="-v $volume:/rootfs:ro -v $here:/audit:ro -v $scripts:/chromium-build-scripts:ro"
args=""
while [ $# -gt 0 ]; do
    case $1 in
        --trace|--json)
            host=$2
            mkdir -p "$(dirname -- "$host")"
            dir=$(CDPATH= cd -- "$(dirname -- "$host")" && pwd)
            case $1 in
                --trace) mounts="$mounts -v $dir/$(basename -- "$host"):/in/trace.tsv:ro"; args="$args --trace /in/trace.tsv" ;;
                --json) mounts="$mounts -v $dir:/out"; args="$args --json /out/$(basename -- "$host")" ;;
            esac
            shift 2 ;;
        --deny-gaps) args="$args --deny-gaps"; shift ;;
        *) echo "audit.sh: unknown option $1" >&2; exit 2 ;;
    esac
done

# shellcheck disable=SC2086
docker run --rm $mounts alpine:3.22 sh -c "apk add -q python3 binutils > /dev/null && python3 -I -B /audit/census.py /rootfs --table /audit/attribution.toml --chromium-build-scripts /chromium-build-scripts --readelf readelf $args"
