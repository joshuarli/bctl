#!/usr/bin/env python3
"""Attribute every file of an image rootfs to Rust-owned, a named exception, or a gap."""

from __future__ import annotations

import argparse
import fnmatch
import importlib.util
import json
import os
import posixpath
import re
import stat
import sys
import tomllib
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

CLASSES = ("rust", "exception", "gap")
CONSUMER_LIMIT = 6
MAX_SYMLINK_HOPS = 8


class CensusError(RuntimeError):
    pass


def load_linux_runtime(scripts: Path):
    module_path = scripts / "linux_runtime.py"
    if not module_path.is_file():
        raise CensusError(f"no linux_runtime.py in {scripts}")
    spec = importlib.util.spec_from_file_location("linux_runtime", module_path)
    module = importlib.util.module_from_spec(spec)
    sys.modules["linux_runtime"] = module
    spec.loader.exec_module(module)
    return module


@dataclass(frozen=True)
class Rule:
    klass: str
    name: str
    reason: str
    source: str | None
    paths: tuple[str, ...]
    packages: frozenset[str]
    elf: bool | None
    unit_per_file: bool


def load_rules(path: Path) -> tuple[list[Rule], tuple[str, ...]]:
    with path.open("rb") as stream:
        raw = tomllib.load(stream)
    unknown = set(raw) - {"rule", "exclude"}
    if unknown:
        raise CensusError(f"{path}: unknown keys {sorted(unknown)}")
    rules = []
    for index, entry in enumerate(raw.get("rule", []), 1):
        where = f"{path}: rule {index} ({entry.get('name', '?')})"
        unknown = set(entry) - {"class", "name", "reason", "source", "paths", "packages", "elf", "unit"}
        if unknown:
            raise CensusError(f"{where}: unknown keys {sorted(unknown)}")
        klass = entry.get("class")
        if klass not in CLASSES:
            raise CensusError(f"{where}: class must be one of {CLASSES}")
        for key in ("name", "reason"):
            if not entry.get(key):
                raise CensusError(f"{where}: missing {key}")
        if klass == "rust" and not entry.get("source"):
            raise CensusError(f"{where}: a rust rule names its crate source")
        if klass != "rust" and "source" in entry:
            raise CensusError(f"{where}: only rust rules carry a source")
        if not entry.get("paths") and not entry.get("packages"):
            raise CensusError(f"{where}: no paths or packages")
        if "elf" in entry and not isinstance(entry["elf"], bool):
            raise CensusError(f"{where}: elf must be true or false")
        if entry.get("unit", "file") != "file":
            raise CensusError(f"{where}: unit must be \"file\" or absent")
        rules.append(
            Rule(
                klass=klass,
                name=entry["name"],
                reason=entry["reason"],
                source=entry.get("source"),
                paths=tuple(entry.get("paths", ())),
                packages=frozenset(entry.get("packages", ())),
                elf=entry.get("elf"),
                unit_per_file="unit" in entry,
            )
        )
    if not rules:
        raise CensusError(f"{path}: no rules")
    return rules, tuple(raw.get("exclude", ()))


@dataclass
class Entry:
    path: str
    kind: str
    size: int
    key: tuple[int, int]
    package: str | None = None
    rule: Rule | None = None


def walk_rootfs(root: Path) -> list[Entry]:
    entries = []
    for directory, names, files in os.walk(root, followlinks=False):
        for name in (*names, *files):
            full = Path(directory) / name
            info = full.lstat()
            relative = "/" + full.relative_to(root).as_posix()
            if stat.S_ISLNK(info.st_mode):
                entries.append(Entry(relative, "symlink", 0, (info.st_dev, info.st_ino)))
            elif stat.S_ISREG(info.st_mode):
                entries.append(Entry(relative, "file", info.st_size, (info.st_dev, info.st_ino)))
            elif stat.S_ISDIR(info.st_mode):
                continue
            else:
                raise CensusError(f"{relative}: unsupported file type {oct(info.st_mode)}")
    return sorted(entries, key=lambda e: e.path)


@dataclass
class ApkDb:
    owner: dict[str, str]
    dependents: dict[str, set[str]]


def parse_apk_db(root: Path) -> ApkDb | None:
    db = root / "lib/apk/db/installed"
    if not db.is_file():
        return None
    packages: list[dict] = []
    current: dict | None = None
    directory = ""
    for line in db.read_text(encoding="utf-8").splitlines():
        if not line:
            current = None
            continue
        tag, _, value = line.partition(":")
        if current is None:
            current = {"name": None, "files": [], "depends": [], "provides": []}
            packages.append(current)
            directory = ""
        if tag == "P":
            current["name"] = value
        elif tag == "F":
            directory = value
        elif tag == "R":
            current["files"].append(f"/{directory}/{value}".replace("//", "/"))
        elif tag == "D":
            current["depends"] = value.split()
        elif tag == "p":
            current["provides"] = value.split()
    owner: dict[str, str] = {}
    provides: dict[str, str] = {}
    for package in packages:
        for path in package["files"]:
            owner[path] = package["name"]
        provides[package["name"]] = package["name"]
        for item in package["provides"]:
            provides.setdefault(re.split(r"[=<>~]", item)[0], package["name"])
    dependents: dict[str, set[str]] = defaultdict(set)
    for package in packages:
        for item in package["depends"]:
            if item.startswith("!"):
                continue
            target = provides.get(re.split(r"[=<>~]", item)[0])
            if target and target != package["name"]:
                dependents[target].add(package["name"])
    return ApkDb(owner, dependents)


def symlink_owner(root: Path, path: str, db: ApkDb) -> str | None:
    for _ in range(MAX_SYMLINK_HOPS):
        full = root / path.lstrip("/")
        if not full.is_symlink():
            return db.owner.get(path)
        target = os.readlink(full)
        path = posixpath.normpath(target if target.startswith("/") else posixpath.join(posixpath.dirname(path), target))
        if path in db.owner:
            return db.owner[path]
    return None


def has_elf_magic(root: Path, entry: Entry) -> bool:
    if entry.kind != "file":
        return False
    with (root / entry.path.lstrip("/")).open("rb") as stream:
        return stream.read(4) == b"\x7fELF"


def attribute(root: Path, entries: list[Entry], rules: list[Rule], db: ApkDb | None) -> list[Entry]:
    unattributed = []
    for entry in entries:
        entry.package = db.owner.get(entry.path) if db else None
        if db and entry.package is None and entry.kind == "symlink":
            entry.package = symlink_owner(root, entry.path, db)
        for rule in rules:
            if rule.elf is not None and has_elf_magic(root, entry) != rule.elf:
                continue
            if any(fnmatch.fnmatchcase(entry.path, pattern) for pattern in rule.paths) or (
                entry.package is not None and entry.package in rule.packages
            ):
                entry.rule = rule
                break
        else:
            unattributed.append(entry)
    return unattributed


def unit_of(entry: Entry) -> str:
    if entry.package:
        return entry.package
    return PurePosixPath(entry.path).name if entry.rule.unit_per_file else entry.rule.name


def classify_elf(info, runtime) -> str:
    if info.relative.name in runtime.MUSL_LIBRARIES:
        return "loader"
    if info.interpreter is not None:
        return "dynamic"
    if info.elf_type.startswith("EXEC"):
        return "static"
    if info.needed or info.soname:
        return "library"
    flags = re.search(r"\(FLAGS_1\).*", runtime.readelf_output(info.path))
    return "static" if flags and "PIE" in flags.group(0) else "library"


def inspect_elfs(root: Path, entries: list[Entry], runtime) -> dict[str, object]:
    infos = {}
    for entry in entries:
        if entry.kind != "file":
            continue
        full = root / entry.path.lstrip("/")
        if runtime.is_elf(full):
            infos[entry.path] = runtime.inspect_elf(full, root)
    return infos


def soname_index(infos: dict[str, object]) -> dict[str, str]:
    index: dict[str, str] = {}
    for path, info in infos.items():
        names = {info.soname, info.relative.name} - {None}
        for name in names:
            index.setdefault(name, path)
    return index


def static_closure(start: str, infos: dict[str, object], index: dict[str, str]) -> set[str]:
    seen: set[str] = set()
    pending = [start]
    while pending:
        path = pending.pop()
        if path in seen or path not in infos:
            continue
        seen.add(path)
        info = infos[path]
        if info.interpreter:
            pending.append(index.get(PurePosixPath(info.interpreter).name, ""))
        for name in info.needed:
            pending.append(index.get(name, ""))
    return seen


def elf_findings(infos: dict[str, object], index: dict[str, str], runtime) -> list[str]:
    findings = []
    for path, info in sorted(infos.items()):
        for name in info.needed:
            if name in runtime.GLIBC_LIBRARIES or name.startswith("ld-linux"):
                findings.append(f"{path}: needs glibc library {name}")
            elif name not in runtime.MUSL_LIBRARIES and name not in index:
                findings.append(f"{path}: NEEDED {name} is not in the image")
        if info.interpreter and info.interpreter not in set(runtime.MUSL_INTERPRETERS.values()):
            findings.append(f"{path}: non-musl interpreter {info.interpreter}")
    return findings


def read_trace(path: Path) -> dict[str, set[str]]:
    mapped: dict[str, set[str]] = defaultdict(set)
    for number, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        if not line.strip():
            continue
        exe, tab, target = line.partition("\t")
        if not tab:
            raise CensusError(f"{path}:{number}: expected 'exe<TAB>path'")
        target = re.sub(r" \(deleted\)$", "", target)
        mapped[exe].add(target)
    if not mapped:
        raise CensusError(f"{path}: empty trace")
    return mapped


def trace_report(mapped, entries, infos, index) -> dict[str, dict]:
    by_path = {entry.path: entry for entry in entries}
    report: dict[str, dict] = {}
    for exe, targets in sorted(mapped.items()):
        if exe not in infos:
            continue
        closure = static_closure(exe, infos, index)
        loaded = sorted(t for t in targets if t in infos and t not in closure and t != exe)
        data = sorted(t for t in targets if t not in infos and t != exe)
        report[exe] = {
            "dlopened": [
                {"path": t, "unit": unit_of(by_path[t]) if t in by_path else None, "class": by_path[t].rule.klass if t in by_path else None}
                for t in loaded
            ],
            "mapped_data": data,
            "data_units": {t: unit_of(by_path[t]) for t in data if t in by_path},
            "unresolved": sorted(t for t in targets if t not in by_path and t != exe),
        }
    return report


def build_report(root: Path, table: tuple[list[Rule], tuple[str, ...]], trace: dict[str, set[str]] | None, runtime) -> dict:
    rules, exclude = table
    entries = [
        e for e in walk_rootfs(root) if not any(fnmatch.fnmatchcase(e.path, pattern) for pattern in exclude)
    ]
    db = parse_apk_db(root)
    unattributed = attribute(root, entries, rules, db)
    if unattributed:
        by_package: dict[str, list[str]] = defaultdict(list)
        for entry in unattributed:
            by_package[entry.package or "(no package)"].append(entry.path)
        lines = [f"{len(unattributed)} unattributed files:"]
        for package, paths in sorted(by_package.items()):
            lines.append(f"  {package}: {len(paths)} files, e.g. {paths[0]}")
        raise CensusError("\n".join(lines))
    infos = inspect_elfs(root, entries, runtime)
    index = soname_index(infos)
    by_path = {entry.path: entry for entry in entries}

    units: dict[str, dict] = {}
    seen_inodes: set[tuple[int, int]] = set()
    for entry in entries:
        unit = units.setdefault(
            unit_of(entry),
            {"unit": unit_of(entry), "class": entry.rule.klass, "rule": entry.rule.name, "reason": entry.rule.reason,
             "source": entry.rule.source, "bytes": 0, "files": 0, "consumers": set(), "elf": 0},
        )
        if unit["class"] != entry.rule.klass:
            raise CensusError(f"package {unit['unit']} is split across classes by the table")
        unit["files"] += 1
        if entry.key not in seen_inodes:
            seen_inodes.add(entry.key)
            unit["bytes"] += entry.size

    for path, info in infos.items():
        units[unit_of(by_path[path])]["elf"] += 1
        for name in info.needed:
            provider = index.get(name)
            if provider and unit_of(by_path[provider]) != unit_of(by_path[path]):
                units[unit_of(by_path[provider])]["consumers"].add(unit_of(by_path[path]))
    if trace:
        for exe, targets in trace.items():
            if exe not in by_path:
                continue
            for target in targets:
                if target in by_path and unit_of(by_path[target]) != unit_of(by_path[exe]):
                    units[unit_of(by_path[target])]["consumers"].add(unit_of(by_path[exe]))
    if db:
        for unit in units.values():
            unit["consumers"] |= db.dependents.get(unit["unit"], set()) - {unit["unit"]}

    elf_rows = []
    for path, info in sorted(infos.items()):
        elf_rows.append(
            {
                "path": path,
                "unit": unit_of(by_path[path]),
                "class": by_path[path].rule.klass,
                "kind": classify_elf(info, runtime),
                "type": info.elf_type,
                "interpreter": info.interpreter,
                "needed": list(info.needed),
                "soname": info.soname,
            }
        )
    traced = trace_report(trace, entries, infos, index) if trace else None
    traced_paths = {p for mapped in trace.values() for p in mapped} if trace else set()
    for unit in units.values():
        unit["consumers"] = sorted(unit["consumers"])
        unit["mapped_in_trace"] = any(
            unit_of(by_path[p]) == unit["unit"] for p in traced_paths if p in by_path
        ) if trace else None
    return {
        "root": str(root),
        "files": len(entries),
        "bytes": sum(u["bytes"] for u in units.values()),
        "units": sorted(units.values(), key=lambda u: (-u["bytes"], u["unit"])),
        "elf": elf_rows,
        "elf_findings": elf_findings(infos, index, runtime),
        "trace": traced,
    }


def human(count: int) -> str:
    value = float(count)
    for suffix in ("B", "KiB", "MiB", "GiB"):
        if value < 1024 or suffix == "GiB":
            return f"{int(value)} {suffix}" if suffix == "B" else f"{value:.1f} {suffix}"
        value /= 1024
    raise AssertionError


def consumers_text(consumers: list[str]) -> str:
    if not consumers:
        return "none"
    shown = ", ".join(consumers[:CONSUMER_LIMIT])
    return shown + (f" (+{len(consumers) - CONSUMER_LIMIT})" if len(consumers) > CONSUMER_LIMIT else "")


def render_markdown(report: dict) -> str:
    out = []
    totals: dict[str, list[int]] = defaultdict(lambda: [0, 0, 0])
    for unit in report["units"]:
        row = totals[unit["class"]]
        row[0] += 1
        row[1] += unit["files"]
        row[2] += unit["bytes"]
    out.append("| class | units | files | bytes |\n|---|---:|---:|---:|")
    for klass in CLASSES:
        units, files, size = totals[klass]
        out.append(f"| {klass} | {units} | {files} | {human(size)} |")
    out.append(f"| total | {sum(t[0] for t in totals.values())} | {report['files']} | {human(report['bytes'])} |")

    for klass, title in (("rust", "Rust-owned"), ("exception", "Named exceptions"), ("gap", "Gaps")):
        rows = [u for u in report["units"] if u["class"] == klass]
        if not rows:
            out.append(f"\n### {title}\n\nNone.")
            continue
        out.append(f"\n### {title}\n")
        if klass == "gap":
            out.append("| unit | bytes | files | ELF | in trace | consumers | why it is a gap |\n|---|---:|---:|---:|---|---|---|")
            for u in rows:
                traced = {True: "yes", False: "no", None: "n/a"}[u["mapped_in_trace"]]
                out.append(f"| {u['unit']} | {human(u['bytes'])} | {u['files']} | {u['elf']} | {traced} | {consumers_text(u['consumers'])} | {u['reason']} |")
        else:
            out.append("| unit | rule | bytes | files | ELF | basis |\n|---|---|---:|---:|---:|---|")
            for u in rows:
                basis = u["source"] if klass == "rust" else u["reason"]
                out.append(f"| {u['unit']} | {u['rule']} | {human(u['bytes'])} | {u['files']} | {u['elf']} | {basis} |")

    elf = report["elf"]
    kinds = defaultdict(int)
    for row in elf:
        kinds[row["kind"]] += 1
    out.append("\n### ELF files\n")
    out.append(f"{len(elf)} ELF files: " + ", ".join(f"{count} {kind}" for kind, count in sorted(kinds.items())) + ".")
    out.append("")
    out.append("Static ELF executables (no PT_INTERP):\n")
    static = [r for r in elf if r["kind"] == "static"]
    out.extend([f"- `{r['path']}` ({r['unit']}, {r['class']})" for r in static] or ["- none"])
    out.append("\nELF audit findings (glibc dependencies, missing libraries, foreign interpreters):\n")
    out.extend([f"- {f}" for f in report["elf_findings"]] or ["- none"])

    if report["trace"] is not None:
        out.append("\n### Runtime loads from the traced B1 run\n")
        out.append(
            "Libraries mapped by a process but outside its static DT_NEEDED closure, "
            "which means a `dlopen` by it or by a library it loaded. Processes with none are omitted.\n"
        )
        for exe, data in report["trace"].items():
            if not data["dlopened"]:
                continue
            out.append(f"`{exe}`:\n")
            out.append("| library | unit | class |\n|---|---|---|")
            out.extend(f"| `{d['path']}` | {d['unit']} | {d['class']} |" for d in data["dlopened"])
            out.append("")
        out.append("Data files mapped by `exe`, by unit:\n")
        for exe, data in report["trace"].items():
            counts = defaultdict(int)
            for item in data["mapped_data"]:
                counts[data["data_units"].get(item, "created at run time")] += 1
            if counts:
                out.append(f"- `{exe}`: " + ", ".join(f"{unit} {count}" for unit, count in sorted(counts.items())))
    return "\n".join(out) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="extracted image rootfs")
    parser.add_argument("--table", type=Path, default=Path(__file__).with_name("attribution.toml"))
    parser.add_argument("--trace", type=Path, help="TSV of 'exe<TAB>mapped path' sampled from /proc/*/maps")
    parser.add_argument("--chromium-build-scripts", type=Path, required=True)
    parser.add_argument("--readelf", default=None, help="readelf binary (LLVM_READELF)")
    parser.add_argument("--json", type=Path, help="write the full report as JSON")
    parser.add_argument("--deny-gaps", action="store_true", help="exit 3 when any gap remains")
    args = parser.parse_args()
    if args.readelf:
        os.environ["LLVM_READELF"] = args.readelf
    try:
        runtime = load_linux_runtime(args.chromium_build_scripts)
    except CensusError as error:
        print(f"census: {error}", file=sys.stderr)
        return 1
    try:
        report = build_report(
            args.root.resolve(),
            load_rules(args.table),
            read_trace(args.trace) if args.trace else None,
            runtime,
        )
    except (CensusError, runtime.RuntimeAuditError) as error:
        print(f"census: {error}", file=sys.stderr)
        return 1
    if args.json:
        args.json.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8")
    sys.stdout.write(render_markdown(report))
    if args.deny_gaps and any(u["class"] == "gap" for u in report["units"]):
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
