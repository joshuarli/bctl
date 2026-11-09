#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$root/.work/tmp"
export TMPDIR="$root/.work/tmp"

exec python3 -B "$root/scripts/update_smolvm.py" --repo-root "$root" "$@"
