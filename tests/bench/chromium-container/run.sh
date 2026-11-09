#!/bin/sh
set -eu
sandbox=${BENCH_CHROMIUM_FLAGS:-}
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp &
until [ -e /tmp/.X11-unix/X99 ]; do sleep 0.1; done
socat TCP-LISTEN:9223,fork,reuseaddr TCP:127.0.0.1:9222 &
exec su-exec browser env DISPLAY=:99 chromium --ozone-platform=x11 $sandbox \
    --remote-debugging-port=9222 --user-data-dir=/tmp/profile --no-first-run \
    --no-default-browser-check --window-size=1280,800 --start-maximized about:blank
