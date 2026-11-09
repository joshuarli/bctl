#!/bin/sh
out=/run/r0-maps.tsv
while :; do
    awk 'FNR == 1 { exe = "" } $6 ~ /^\// { if (exe == "") exe = $6; print exe "\t" $6 }' /proc/[0-9]*/maps 2>/dev/null | sort -u > "$out.new" || true
    sort -u "$out.new" "$out" 2>/dev/null > "$out.merge" || true
    mv "$out.merge" "$out"
    sleep 0.3
done
