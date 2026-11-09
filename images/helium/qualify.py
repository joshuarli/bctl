import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

LOADER = PurePosixPath("/lib/ld-musl-aarch64.so.1")
SYSTEM_PATH = (PurePosixPath("/lib"), PurePosixPath("/usr/local/lib"), PurePosixPath("/usr/lib"))
BROWSER_ROOT = PurePosixPath("/opt/browser")
BROWSER_LIBRARY_PATH = (BROWSER_ROOT / "lib",)
XKBCOMMON = PurePosixPath("/usr/local/lib/libxkbcommon.so.0")
MAX_SYMLINK_HOPS = 16


class Refusal(RuntimeError):
    pass


@dataclass(frozen=True)
class Elf:
    path: PurePosixPath
    machine: str
    elf_class: str
    interpreter: PurePosixPath | None
    needed: tuple[str, ...]
    search: tuple[PurePosixPath, ...]
    defined: frozenset[str]
    undefined: frozenset[str]


def readelf(host: Path, *flags: str) -> str:
    result = subprocess.run(["readelf", "-W", *flags, str(host)], capture_output=True, text=True, check=False)
    if result.returncode:
        raise Refusal(f"readelf {' '.join(flags)} {host}: {result.stderr.strip()}")
    return result.stdout


def field(output: str, label: str, path: PurePosixPath) -> str:
    match = re.search(rf"^\s*{label}:\s+(.+?)\s*$", output, re.MULTILINE)
    if not match:
        raise Refusal(f"{path}: readelf reports no {label}")
    return match.group(1)


def symbols(output: str) -> tuple[frozenset[str], frozenset[str]]:
    defined, undefined = set(), set()
    for line in output.splitlines():
        parts = line.split()
        if len(parts) < 8 or not parts[0].endswith(":") or parts[3] in {"FILE", "SECTION"}:
            continue
        _, _, _, kind, bind, visibility, index, name = parts[:8]
        name = name.split("@", 1)[0]
        if not name:
            continue
        if index == "UND":
            if bind == "GLOBAL":
                undefined.add(name)
        elif bind in {"GLOBAL", "WEAK", "UNIQUE"} and visibility in {"DEFAULT", "PROTECTED"}:
            defined.add(name)
    return frozenset(defined), frozenset(undefined)


class Rootfs:
    def __init__(self, root: Path):
        self.root = root
        self.elfs: dict[PurePosixPath, Elf] = {}

    def host(self, path: PurePosixPath) -> Path:
        return self.root / path.relative_to("/")

    def resolve(self, path: PurePosixPath) -> PurePosixPath | None:
        for _ in range(MAX_SYMLINK_HOPS):
            host = self.host(path)
            if host.is_symlink():
                target = PurePosixPath(os.readlink(host))
                path = PurePosixPath(os.path.normpath(target if target.is_absolute() else path.parent / target))
                continue
            return path if host.is_file() else None
        raise Refusal(f"{path}: more than {MAX_SYMLINK_HOPS} symlink hops")

    def files(self) -> list[PurePosixPath]:
        found = []
        for directory, _, names in os.walk(self.root):
            for name in names:
                host = Path(directory) / name
                if host.is_symlink() or not host.is_file():
                    continue
                with host.open("rb") as stream:
                    if stream.read(4) == b"\x7fELF":
                        found.append(PurePosixPath("/") / host.relative_to(self.root).as_posix())
        return sorted(found)

    def load(self, path: PurePosixPath) -> Elf:
        if path in self.elfs:
            return self.elfs[path]
        host = self.host(path)
        headers = readelf(host, "-h", "-l", "-d")
        interpreter = re.search(r"\[Requesting program interpreter: ([^\]]+)\]", headers)
        rpath = re.search(r"\((?:RPATH|RUNPATH)\).*?\[([^\]]*)\]", headers)
        entries = rpath.group(1).split(":") if rpath and rpath.group(1) else []
        dynamic = []
        for entry in entries:
            expanded = entry.replace("${ORIGIN}", str(path.parent)).replace("$ORIGIN", str(path.parent))
            if "$" in expanded or not expanded.startswith("/"):
                raise Refusal(f"{path}: unsupported dynamic path {entry!r}")
            dynamic.append(PurePosixPath(os.path.normpath(expanded)))
        library_path = BROWSER_LIBRARY_PATH if BROWSER_ROOT in path.parents else ()
        defined, undefined = symbols(readelf(host, "--dyn-syms"))
        elf = Elf(
            path=path,
            machine=field(headers, "Machine", path),
            elf_class=field(headers, "Class", path),
            interpreter=PurePosixPath(interpreter.group(1)) if interpreter else None,
            needed=tuple(re.findall(r"\(NEEDED\).*?Shared library: \[([^\]]+)\]", headers)),
            search=(*library_path, *dynamic, *SYSTEM_PATH),
            defined=defined,
            undefined=undefined,
        )
        self.elfs[path] = elf
        return elf

    def find(self, elf: Elf, name: str) -> PurePosixPath | None:
        if "/" in name:
            raise Refusal(f"{elf.path}: DT_NEEDED {name} is a path")
        for directory in elf.search:
            resolved = self.resolve(directory / name)
            if resolved is not None:
                return resolved
        return None

    def closure(self, root: Elf, missing: list[str]) -> list[Elf]:
        order, seen, pending = [], set(), [root]
        while pending:
            elf = pending.pop(0)
            if elf.path in seen:
                continue
            seen.add(elf.path)
            order.append(elf)
            for name in elf.needed:
                found = self.find(elf, name)
                if found is None:
                    missing.append(f"{elf.path}: DT_NEEDED {name} is not on its search path {[str(d) for d in elf.search]}")
                else:
                    pending.append(self.load(found))
        return order


def qualify(root: Path) -> dict:
    rootfs = Rootfs(root)
    failures: list[str] = []
    paths = rootfs.files()
    for path in paths:
        elf = rootfs.load(path)
        if elf.elf_class != "ELF64" or elf.machine != "AArch64":
            failures.append(f"{path}: {elf.elf_class} {elf.machine}, not ELF64 AArch64")
        if elf.interpreter is not None:
            if elf.interpreter != LOADER:
                failures.append(f"{path}: interpreter {elf.interpreter}, not {LOADER}")
            elif rootfs.resolve(elf.interpreter) is None:
                failures.append(f"{path}: interpreter {elf.interpreter} is missing")
    unresolved: dict[str, list[str]] = {}
    for path in paths:
        missing: list[str] = []
        closure = rootfs.closure(rootfs.elfs[path], missing)
        failures.extend(missing)
        provided = frozenset().union(*(elf.defined for elf in closure))
        for elf in closure:
            for name in sorted(elf.undefined - provided):
                unresolved.setdefault(f"{elf.path} (loaded by {path})" if elf.path != path else str(path), []).append(name)
    for where, names in unresolved.items():
        failures.append(f"{where}: imports defined nowhere in its closure: {', '.join(names)}")
    xkb_exports = sorted(name for name in rootfs.load(XKBCOMMON).defined if name.startswith("xkb_"))
    xkb_imports = {
        str(path): sorted(name for name in rootfs.elfs[path].undefined if name.startswith("xkb_"))
        for path in paths
        if any(name.startswith("xkb_") for name in rootfs.elfs[path].undefined)
    }
    for path, names in xkb_imports.items():
        absent = sorted(set(names) - set(xkb_exports))
        if absent:
            failures.append(f"{path}: imports xkb symbols {XKBCOMMON} does not export: {', '.join(absent)}")
    return {
        "elf_files": len(paths),
        "no_dt_needed": sorted(str(p) for p in paths if rootfs.elfs[p].interpreter is None and not rootfs.elfs[p].needed),
        "xkbcommon_exports": xkb_exports,
        "xkbcommon_imports": xkb_imports,
        "failures": failures,
    }


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: qualify.py ROOTFS", file=sys.stderr)
        return 2
    try:
        report = qualify(Path(sys.argv[1]))
    except Refusal as error:
        print(f"qualify.py: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 1 if report["failures"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
