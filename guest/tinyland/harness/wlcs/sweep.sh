#!/bin/sh
set -u
module=$1
wlcs=/wlcs-build/wlcs
log=${2:-/tmp/wlcs-sweep}
mkdir -p "$log"
"$wlcs" "$module" --gtest_list_tests | awk '
    /^[^ ]/ { suite = $1; next }
    { name = $1; print suite name }
' > "$log/tests"
while read -r test; do
    case $test in
        *DISABLED_*) printf '%s %s\n' DISABLED "$test"; continue ;;
    esac
    safe=$(printf %s "$test" | tr '/' '_')
    timeout 20 "$wlcs" "$module" --gtest_filter="$test" > "$log/$safe.out" 2>&1
    code=$?
    if [ "$code" -eq 0 ]; then
        if grep -q '^\[  SKIPPED \]' "$log/$safe.out"; then
            status=SKIP
        else
            status=PASS
        fi
    elif [ "$code" -eq 124 ]; then
        status=TIMEOUT
    elif [ "$code" -gt 128 ]; then
        status=CRASH
    else
        status=FAIL
    fi
    printf '%s %s\n' "$status" "$test"
done < "$log/tests"
