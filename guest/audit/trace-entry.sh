#!/bin/sh
/usr/local/bin/r0-trace-sampler &
exec /usr/local/bin/bctl-guest "$@"
