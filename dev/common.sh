ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$ROOT/dev/toolchain.lock"
WORK=$ROOT/.work
TOOLCHAIN=$WORK/toolchain
LLVM=$TOOLCHAIN/$LLVM_DIR
SYSROOT=$WORK/sysroot
GOROOT=$TOOLCHAIN/go
NANOS=$ROOT/nanos
NANOS_OUT=$NANOS/output
TMPDIR=$WORK/tmp
export TMPDIR

die() { printf '%s: %s\n' "$(basename -- "$0")" "$*" >&2; exit 1; }

ensure_work() {
	exclude=$(git -C "$ROOT" rev-parse --git-path info/exclude)
	case $exclude in /*) ;; *) exclude=$ROOT/$exclude ;; esac
	mkdir -p "$(dirname -- "$exclude")"
	grep -qx '\.work/' "$exclude" 2>/dev/null || printf '.work/\n' >>"$exclude"
	mkdir -p "$WORK/dl" "$WORK/tmp" "$TOOLCHAIN"
}
