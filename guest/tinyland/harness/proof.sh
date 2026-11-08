#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash harness/build-guest.sh 2>&1 | tail -n 5
exec bash harness/run-proof.sh "$@"
