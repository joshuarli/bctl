#!/usr/bin/env bash
set -euo pipefail

dir=${1:?directory}
arch=${2:?architecture}
tools=${3:-${dir}/bin}
readelf="${tools}/llvm-readelf"
nm="${tools}/llvm-nm"
[ -x "$readelf" ] && [ -x "$nm" ] || { echo "check-purity: no llvm-readelf/llvm-nm in ${tools}" >&2; exit 2; }

failures=0
checked=0
fail() { echo "IMPURE: $1: $2"; failures=$((failures + 1)); }

is_elf_or_archive() {
    local magic
    magic=$(head -c 8 "$1" 2>/dev/null | od -An -c | tr -d ' \n')
    case "$magic" in
        177ELF*|'!<arch>'*) return 0 ;;
    esac
    return 1
}

while IFS= read -r -d '' file; do
    is_elf_or_archive "$file" || continue
    checked=$((checked + 1))
    rel=${file#"$dir"/}

    if [ "$(head -c 4 "$file" | od -An -c | tr -d ' \n')" = '177ELF' ]; then
        while IFS= read -r needed; do
            [ -n "$needed" ] || continue
            fail "$rel" "DT_NEEDED ${needed}"
        done < <("$readelf" -d "$file" 2>/dev/null | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
        interp=$("$readelf" -l "$file" 2>/dev/null | sed -n 's/.*\[Requesting program interpreter: \(.*\)\]/\1/p')
        [ -z "$interp" ] || fail "$rel" "interpreter ${interp}"
    fi

    if "$readelf" -p .comment "$file" 2>/dev/null | grep -q 'GCC:'; then
        fail "$rel" ".comment names GCC: $("$readelf" -p .comment "$file" 2>/dev/null | grep -m1 'GCC:' | sed 's/^ *\[ *[0-9a-f]*\] *//')"
    fi

    gnu_symbols=$("$nm" -A "$file" 2>/dev/null |
        grep -E ' (_ZNSt7__cxx11|_ZNKSt7__cxx11)|@GLIBC_|@GLIBCXX_|@GCC_' |
        head -3 || true)
    if [ -n "$gnu_symbols" ]; then
        fail "$rel" "GNU runtime symbols: $(echo "$gnu_symbols" | sed 's/^.*: //' | tr '\n' ' ')"
    fi
done < <(find "$dir" -type f -print0)

echo "check-purity: ${checked} ELF files and archives checked in ${dir}, ${failures} impure"
[ "$failures" -eq 0 ]
