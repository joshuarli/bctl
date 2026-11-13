ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$ROOT/dev/toolchain.lock"
WORK=$ROOT/.work
TOOLCHAIN=$WORK/toolchain
LLVM=$TOOLCHAIN/$LLVM_DIR
SYSROOT=$WORK/sysroot
GOROOT=$TOOLCHAIN/go
NANOS=$ROOT/nanos
NANOS_OUT=$NANOS/output
NANOS_DEBUG=$WORK/build/nanos-debug
TMPDIR=$WORK/tmp
case $(uname -s)/$(uname -m) in
Linux/x86_64) ARCH=x86_64 PLATFORM=pc ;;
Linux/aarch64 | Darwin/arm64) ARCH=aarch64 PLATFORM=virt ;;
*) printf 'unsupported host: %s/%s\n' "$(uname -s)" "$(uname -m)" >&2; exit 1 ;;
esac
MUSL_LOADER=ld-musl-$ARCH.so.1
MUSL_SONAME=libc.musl-$ARCH.so.1
BUILDER=hydrogen/aarch64-builder
KRUN_RUN=$WORK/build/krun-run
VIRTIOFSD=${VIRTIOFSD:-virtiofsd}
CCACHE_DIR=$WORK/ccache
export TMPDIR CCACHE_DIR

sha256_is() { [ -f "$2" ] && [ "$(sha256sum "$2" | cut -d' ' -f1)" = "$1" ]; }

die() { printf '%s: %s\n' "$(basename -- "$0")" "$*" >&2; exit 1; }

ensure_work() {
	if exclude=$(git -C "$ROOT" rev-parse --git-path info/exclude 2>/dev/null); then
		case $exclude in /*) ;; *) exclude=$ROOT/$exclude ;; esac
		mkdir -p "$(dirname -- "$exclude")"
		grep -qx '\.work/' "$exclude" 2>/dev/null || printf '.work/\n' >>"$exclude"
	fi
	mkdir -p "$WORK/dl" "$WORK/tmp" "$TOOLCHAIN"
}

ensure_builder() {
	docker build --platform linux/arm64 -q -t "$BUILDER" -f "$ROOT/dev/aarch64.Dockerfile" "$ROOT/dev" >/dev/null ||
		die "cannot build the $BUILDER image"
}

linux() {
	if [ "$(uname -s)" = Darwin ]; then
		set -- -v "$ROOT:$ROOT" -w "$PWD" "$BUILDER" "$@"
		[ ! -f "$SYSROOT/lib/libc.so" ] || set -- -v "$SYSROOT/lib/libc.so:/lib/$MUSL_LOADER:ro" "$@"
		docker run --rm --platform linux/arm64 --security-opt seccomp=unconfined --cap-add SYS_PTRACE "$@"
	else
		"$@"
	fi
}
