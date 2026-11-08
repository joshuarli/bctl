#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash harness/build-guest.sh 2>&1 | tail -n 5
bash harness/run-proof.sh drm-proof
bash harness/run-proof.sh drm-proof atomic-unsupported
python3 -B - <<'PY'
import importlib.util
spec = importlib.util.spec_from_file_location('screenshot', 'harness/check-screenshot.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
atomic = module.read_png_rgb('.work/drm-proof.png')
legacy = module.read_png_rgb('.work/drm-proof-atomic-unsupported.png')
assert atomic == legacy, 'atomic and legacy scanout pixels differ'
print('DRM_PARITY_OK: atomic-versus-legacy modesetting parity, identical screenshots via shared page-flip presentation')
PY
bash harness/run-proof.sh smoke atomic-unsupported
