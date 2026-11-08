#!/bin/bash
set -euo pipefail
exec bash "$(dirname "$0")/proof.sh" smoke
