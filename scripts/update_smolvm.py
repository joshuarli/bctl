from __future__ import annotations

import argparse
import configparser
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Dict, List, Optional, Sequence, Tuple


SMOLVM_URL = "https://github.com/smol-machines/smolvm.git"
KERNEL_URL = "https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git"
KERNEL_ARCHIVE_BASE = "https://cdn.kernel.org/pub/linux/kernel"
KERNEL_MIRROR = "https://github.com/gregkh/linux.git"
LFS_POINTER = b"version https://git-lfs.github.com/spec/v1"
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
RELEASE = re.compile(r"^v([0-9]+)\.([0-9]+)\.([0-9]+)$")
KERNEL_VERSION = re.compile(r"^linux-([0-9]+)\.([0-9]+)\.([0-9]+)$")
KNOWN_GITLINKS = {"libkrun", "libkrunfw"}
SOURCE_TREES = ("smolvm", "libkrun", "libkrunfw")
EXCLUDED_PATHS = {
    "smolvm": {
        ".github",
        ".oss-scanner",
        "assets",
        "bench",
        "demo",
        "deploy",
        "docs",
        "examples",
        "lib",
        "nix",
        "packaging",
    },
    "libkrun": {
        ".github",
        ".claude",
        ".gemini",
        "examples",
    },
    "libkrunfw": {
        ".github",
        "CODEOWNERS",
        "initrd",
        "patches-tee",
        "qboot",
        "build_on_krunvm.sh",
        "build_on_krunvm_debian.sh",
        "build_on_krunvm_fedora.sh",
    },
}
REQUIRED_FILES = (
    "LICENSE",
    "THIRD-PARTY-NOTICES.md",
    "Cargo.toml",
    "Cargo.lock",
    "smolvm.entitlements",
    "scripts/smolvm-wrapper.sh",
    "scripts/build-virglrenderer-macos.sh",
    "patches/virglrenderer/0001-macos-venus-bridge.patch",
    "libkrun/LICENSE",
    "libkrun/Cargo.toml",
    "libkrun/Cargo.lock",
    "libkrun/src/rutabaga_gfx/LICENSE",
    "libkrunfw/LICENSE-GPL-2.0-only",
    "libkrunfw/LICENSE-LGPL-2.1-only",
    "libkrunfw/Makefile",
    "libkrunfw/bin2cbundle.py",
    "libkrunfw/config-libkrunfw_aarch64",
)


class UpdateError(Exception):
    pass


def run(command: Sequence[str], cwd: Optional[Path] = None, timeout: int = 180) -> str:
    try:
        result = subprocess.run(
            list(command),
            cwd=str(cwd) if cwd else None,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        raise UpdateError(f"{' '.join(command)} timed out after {timeout} seconds") from error
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() or error.stdout.strip()
        raise UpdateError(f"{' '.join(command)} failed: {detail}") from error
    return result.stdout.strip()


def require_repo(path: Path, name: str) -> Path:
    resolved = path.resolve(strict=True)
    if not resolved.is_dir() or not (resolved / ".git").exists():
        raise UpdateError(f"{name} source is not a Git checkout: {path}")
    top = Path(run(["git", "-C", str(resolved), "rev-parse", "--show-toplevel"]))
    if top.resolve(strict=True) != resolved:
        raise UpdateError(f"{name} source path is not its Git root: {path}")
    status = run(["git", "-C", str(resolved), "status", "--porcelain", "--untracked-files=all"])
    if status:
        raise UpdateError(f"{name} source checkout is dirty:\n{status}")
    return resolved


def require_repo_root(path: Path) -> Path:
    resolved = path.resolve(strict=True)
    top = Path(run(["git", "-C", str(resolved), "rev-parse", "--show-toplevel"]))
    if top.resolve(strict=True) != resolved:
        raise UpdateError(f"repository root is not the Git worktree root: {path}")
    return resolved


def require_ignored_work(repo_root: Path) -> None:
    if subprocess.run(
        ["git", "-C", str(repo_root), "check-ignore", "-q", "--", ".work/tmp/probe"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    ).returncode == 0:
        return
    exclude_path = Path(
        run(["git", "-C", str(repo_root), "rev-parse", "--path-format=absolute", "--git-path", "info/exclude"])
    )
    if exclude_path.is_symlink():
        raise UpdateError(f"refusing a symlinked Git exclude file: {exclude_path}")
    exclude_path.parent.mkdir(parents=True, exist_ok=True)
    if exclude_path.exists() and not exclude_path.is_file():
        raise UpdateError(f"Git exclude path is not a regular file: {exclude_path}")
    lines = exclude_path.read_text().splitlines() if exclude_path.exists() else []
    if ".work/" not in lines:
        with exclude_path.open("a", encoding="utf-8") as output:
            if lines and exclude_path.read_bytes()[-1:] != b"\n":
                output.write("\n")
            output.write(".work/\n")
    if subprocess.run(
        ["git", "-C", str(repo_root), "check-ignore", "-q", "--", ".work/tmp/probe"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    ).returncode != 0:
        raise UpdateError(f".work is not ignored by the local Git exclude file: {exclude_path}")


def head_revision(repo: Path) -> str:
    revision = run(["git", "-C", str(repo), "rev-parse", "HEAD"])
    if not HEX_40.fullmatch(revision):
        raise UpdateError(f"invalid Git revision in {repo}: {revision}")
    return revision


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def release_parts(release: str) -> Tuple[int, int, int]:
    match = RELEASE.fullmatch(release)
    if not match:
        raise UpdateError(f"release must be a stable vX.Y.Z tag: {release}")
    return tuple(int(part) for part in match.groups())


def parse_remote_head(output: str) -> Tuple[str, str]:
    branch = ""
    commit = ""
    for line in output.splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[0] == "ref:" and fields[2] == "HEAD":
            ref = fields[1]
            if not ref.startswith("refs/heads/"):
                raise UpdateError(f"unexpected upstream default ref: {ref}")
            branch = ref.removeprefix("refs/heads/")
        elif len(fields) == 2 and fields[1] == "HEAD":
            commit = fields[0]
    if not branch or not HEX_40.fullmatch(commit):
        raise UpdateError("cannot resolve the upstream default branch tip")
    return branch, commit


@dataclass(frozen=True)
class SourceTarget:
    label: str
    revision: str
    commit: Optional[str]
    branch: Optional[str]
    is_release: bool


def default_branch_tip() -> SourceTarget:
    branch, commit = parse_remote_head(run(["git", "ls-remote", "--symref", SMOLVM_URL, "HEAD"]))
    return SourceTarget(
        label=f"{branch}@{commit}",
        revision=commit,
        commit=commit,
        branch=branch,
        is_release=False,
    )


def clone_revision(url: str, revision: str, destination: Path) -> Path:
    run(["git", "clone", "--filter=blob:none", "--no-checkout", url, str(destination)])
    run(["git", "-C", str(destination), "fetch", "--depth=1", "origin", revision])
    run(["git", "-C", str(destination), "checkout", "--detach", "FETCH_HEAD"])
    return require_repo(destination, url)


def gitlinks(repo: Path) -> Dict[str, str]:
    links = {}
    for line in run(["git", "-C", str(repo), "ls-tree", "HEAD"]).splitlines():
        metadata, path = line.split("\t", 1)
        mode, kind, revision = metadata.split()
        if mode == "160000":
            if kind != "commit" or not HEX_40.fullmatch(revision):
                raise UpdateError(f"invalid gitlink in {repo}: {line}")
            links[path] = revision
    if set(links) != KNOWN_GITLINKS:
        raise UpdateError(f"unexpected SmolVM gitlinks: {sorted(links)}")
    return links


def https_url(url: str) -> str:
    if url.startswith("https://"):
        return url
    match = re.fullmatch(r"git@github\.com:([^\s]+)", url)
    if match:
        return "https://github.com/" + match.group(1)
    raise UpdateError(f"unsupported upstream checkout URL: {url}")


def submodule_urls(repo: Path) -> Dict[str, str]:
    config = configparser.ConfigParser(interpolation=None)
    try:
        config.read_string((repo / ".gitmodules").read_text())
    except (OSError, configparser.Error) as error:
        raise UpdateError(f"cannot read SmolVM .gitmodules: {error}") from error
    result = {}
    for section in config.sections():
        if not section.startswith("submodule "):
            continue
        name = section.removeprefix("submodule ").strip('"')
        if "path" not in config[section] or "url" not in config[section]:
            raise UpdateError(f"incomplete submodule record: {section}")
        path = config[section]["path"]
        if path != name:
            raise UpdateError(f"unexpected submodule path {path} for {name}")
        result[path] = https_url(config[section]["url"])
    if set(result) != KNOWN_GITLINKS:
        raise UpdateError(f"unexpected .gitmodules paths: {sorted(result)}")
    return result


def normalize_member(name: str) -> Tuple[str, ...]:
    if "\\" in name:
        raise UpdateError(f"archive path contains a backslash: {name!r}")
    path = PurePosixPath(name)
    if path.is_absolute() or not path.parts:
        raise UpdateError(f"unsafe archive path: {name!r}")
    parts = tuple(part for part in path.parts if part not in ("", "."))
    if not parts or any(part == ".." for part in parts):
        raise UpdateError(f"unsafe archive path: {name!r}")
    return parts


def validate_link(parts: Tuple[str, ...], target: str, name: str) -> None:
    link = PurePosixPath(target)
    if link.is_absolute() or "\\" in target:
        raise UpdateError(f"archive symlink escapes its root: {name} -> {target}")
    resolved = list(parts[:-1])
    for part in link.parts:
        if part in ("", "."):
            continue
        if part == "..":
            if not resolved:
                raise UpdateError(f"archive symlink escapes its root: {name} -> {target}")
            resolved.pop()
        else:
            resolved.append(part)


def excluded(component: str, parts: Tuple[str, ...]) -> bool:
    if parts[-1] in {".gitmodules", ".DS_Store"}:
        return True
    if parts[0] in EXCLUDED_PATHS[component]:
        return True
    if component == "libkrun" and parts[:2] == ("docs", "images"):
        return True
    return False


def extract_git_archive(repo: Path, destination: Path, component: str) -> None:
    archive_path = destination.parent / (destination.name + ".source.tar")
    try:
        with archive_path.open("wb") as archive_file:
            subprocess.run(
                ["git", "-C", str(repo), "archive", "--format=tar", "HEAD"],
                check=True,
                stdout=archive_file,
                stderr=subprocess.PIPE,
            )
        destination.mkdir(parents=True, exist_ok=True)
        seen = set()
        links = []
        with tarfile.open(archive_path, mode="r:") as archive:
            for member in archive:
                parts = normalize_member(member.name)
                if parts in seen:
                    raise UpdateError(f"duplicate archive entry in {component}: {member.name}")
                seen.add(parts)
                if excluded(component, parts):
                    continue
                path = destination.joinpath(*parts)
                if member.isdir():
                    path.mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    path.parent.mkdir(parents=True, exist_ok=True)
                    source = archive.extractfile(member)
                    if source is None:
                        raise UpdateError(f"cannot read archive entry: {member.name}")
                    with source, path.open("xb") as output:
                        first = source.read(128)
                        if first.startswith(LFS_POINTER):
                            raise UpdateError(f"Git LFS pointer found in {component}: {member.name}")
                        output.write(first)
                        shutil.copyfileobj(source, output)
                    path.chmod(0o755 if member.mode & 0o111 else 0o644)
                elif member.issym():
                    validate_link(parts, member.linkname, member.name)
                    links.append((path, member.linkname))
                else:
                    raise UpdateError(f"unsupported archive entry in {component}: {member.name}")
        for path, target in links:
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists() or path.is_symlink():
                raise UpdateError(f"archive path conflicts with a symlink: {path}")
            linked = path.parent / target
            if linked.is_symlink() or not linked.is_file():
                raise UpdateError(
                    f"archive symlink must name a regular file: {path.relative_to(destination)} -> {target}"
                )
            shutil.copyfile(linked, path)
            path.chmod(linked.stat().st_mode & 0o777)
    finally:
        archive_path.unlink(missing_ok=True)


def download(url: str, destination: Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "bctl-source-update"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response, destination.open("wb") as output:
            shutil.copyfileobj(response, output)
    except Exception as error:
        destination.unlink(missing_ok=True)
        raise UpdateError(f"download failed for {url}: {error}") from error


def checksum_for(text: str, filename: str) -> str:
    for line in text.splitlines():
        match = re.match(r"^([0-9a-f]{64})\s+\*?(.+?)\s*$", line)
        if match and Path(match.group(2)).name == filename:
            return match.group(1)
    raise UpdateError(f"published checksum is missing for {filename}")


def kernel_version(libkrunfw: Path) -> Tuple[str, str]:
    text = (libkrunfw / "Makefile").read_text()
    values = re.findall(r"^KERNEL_VERSION\s*=\s*(\S+)\s*$", text, re.MULTILINE)
    if len(values) != 1:
        raise UpdateError("libkrunfw Makefile must declare one KERNEL_VERSION")
    name = values[0]
    match = KERNEL_VERSION.fullmatch(name)
    if not match:
        raise UpdateError(f"unsupported libkrunfw kernel source name: {name}")
    version = ".".join(match.groups())
    return name, version


def kernel_archive_url(version: str) -> str:
    major = version.split(".", 1)[0]
    return f"{KERNEL_ARCHIVE_BASE}/v{major}.x/linux-{version}.tar.xz"


def validate_kernel_archive(path: Path, name: str) -> None:
    notices = set()
    seen = {}
    symlinks = set()
    with tarfile.open(path, mode="r:xz") as archive:
        for member in archive:
            parts = normalize_member(member.name)
            if parts[0] != name:
                raise UpdateError(f"unexpected kernel source archive root: {member.name}")
            if parts in seen:
                raise UpdateError(f"duplicate kernel source archive entry: {member.name}")
            if any(parts[:index] in symlinks for index in range(1, len(parts))):
                raise UpdateError(f"kernel archive entry traverses a symlink: {member.name}")
            if member.issym():
                validate_link(parts[1:], member.linkname, member.name)
                if any(existing[: len(parts)] == parts for existing in seen):
                    raise UpdateError(f"kernel symlink shadows an existing path: {member.name}")
                symlinks.add(parts)
                seen[parts] = "symlink"
            elif member.islnk():
                target = normalize_member(member.linkname)
                if target[0] != name:
                    raise UpdateError(f"kernel hard link escapes its source root: {member.name}")
                seen[parts] = "hardlink"
            elif not (member.isfile() or member.isdir()):
                raise UpdateError(f"unsupported kernel archive entry: {member.name}")
            else:
                seen[parts] = "file" if member.isfile() else "directory"
            if parts == (name, "COPYING"):
                notices.add("COPYING")
            if parts[:2] == (name, "LICENSES"):
                notices.add("LICENSES")
    if notices != {"COPYING", "LICENSES"}:
        raise UpdateError(f"kernel source archive is missing license notices: {path}")


def kernel_revision_for(version: str) -> str:
    tag = "v" + version
    for remote in (KERNEL_URL, KERNEL_MIRROR):
        try:
            output = run(["git", "ls-remote", remote, f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"])
        except UpdateError:
            continue
        records = {}
        for line in output.splitlines():
            revision, ref = line.split()
            records[ref] = revision
        revision = records.get(f"refs/tags/{tag}^{{}}", records.get(f"refs/tags/{tag}", ""))
        if HEX_40.fullmatch(revision):
            return revision
    raise UpdateError(f"cannot resolve the Linux revision for {tag}")


def check_release_tag(repo: Path, release: str) -> None:
    if run(["git", "-C", str(repo), "rev-parse", f"refs/tags/{release}^{{commit}}"]) != head_revision(repo):
        raise UpdateError(f"{release} does not point at the selected SmolVM source commit")


def source_repositories(
    target: SourceTarget, source_root: Optional[Path], work: Path
) -> Tuple[Path, Dict[str, Path], Dict[str, str], Dict[str, str]]:
    if source_root is None:
        smolvm = clone_revision(SMOLVM_URL, target.revision, work / "source-smolvm")
    else:
        smolvm = require_repo(source_root / "smolvm", "SmolVM")
    if target.is_release:
        if source_root is not None:
            check_release_tag(smolvm, target.label)
    elif head_revision(smolvm) != target.commit:
        raise UpdateError(f"the {target.revision} tip moved; rerun the update")
    links = gitlinks(smolvm)
    urls = submodule_urls(smolvm)
    components = {}
    for name in ("libkrun", "libkrunfw"):
        if source_root is None:
            components[name] = clone_revision(urls[name], links[name], work / f"source-{name}")
        else:
            components[name] = require_repo(source_root / name, name)
        if head_revision(components[name]) != links[name]:
            raise UpdateError(f"{name} checkout does not match SmolVM gitlink {links[name]}")
    return smolvm, components, links, urls


def package_version(smolvm: Path) -> str:
    try:
        manifest = tomllib.loads((smolvm / "Cargo.toml").read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise UpdateError(f"cannot read the SmolVM manifest: {error}") from error
    version = manifest.get("package", {}).get("version")
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise UpdateError(f"SmolVM manifest has no stable package version: {version!r}")
    return version


def source_identity(root: Path) -> Tuple[str, Dict[str, str]]:
    entries: Dict[str, str] = {}
    for current, dirs, files in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        for name in list(dirs):
            path = current_path / name
            if path.is_symlink():
                relative = path.relative_to(root).as_posix()
                validate_link(tuple(PurePosixPath(relative).parts), os.readlink(path), relative)
                entries[relative] = "link:" + os.readlink(path)
                dirs.remove(name)
        for name in files:
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            if path.is_symlink():
                target = os.readlink(path)
                validate_link(tuple(PurePosixPath(relative).parts), target, relative)
                entries[relative] = "link:" + target
            else:
                digest = file_sha256(path)
                executable = bool(path.stat().st_mode & 0o111)
                entries[relative] = f"file:{int(executable)}:{digest}"
    digest = hashlib.sha256()
    for relative, identity in sorted(entries.items()):
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(identity.encode("ascii"))
        digest.update(b"\n")
    return "sha256:" + digest.hexdigest(), entries


def validate_layout(root: Path) -> None:
    missing = [name for name in REQUIRED_FILES if not (root / name).is_file()]
    if missing:
        raise UpdateError(f"incomplete imported source layout: {', '.join(missing)}")
    forbidden = {".git", ".gitmodules", ".DS_Store", "target", "kernel.c", "qboot.c", "initrd.c"}
    for path in root.rglob("*"):
        if path.name in forbidden:
            raise UpdateError(f"unexpected source layout entry: {path.relative_to(root)}")
        if path.suffix in {".dylib", ".so", ".dll", ".exe", ".o"}:
            raise UpdateError(f"generated binary in imported source: {path.relative_to(root)}")
        if path.is_file():
            with path.open("rb") as file:
                if file.read(128).startswith(LFS_POINTER):
                    raise UpdateError(f"Git LFS pointer in imported source: {path.relative_to(root)}")
    kernel_cache = root / "libkrunfw" / "tarballs"
    if kernel_cache.is_symlink():
        raise UpdateError("vendored kernel source directory must not be a symlink")
    if kernel_cache.exists() and any(kernel_cache.iterdir()):
        raise UpdateError("kernel source archive must not be included in vendored source")


def patch_records(patch_dir: Path) -> List[Tuple[str, str, str]]:
    patches = sorted(p for tree in SOURCE_TREES for p in patch_dir.glob(f"{tree}-*.patch"))
    if not patches:
        raise UpdateError(f"no ordered source patches found in {patch_dir}")
    records = []
    for patch in patches:
        if not re.match(r"^[a-z0-9]+-[0-9]{4}-", patch.name):
            raise UpdateError(f"source patch name must be <tree>-<order>-<slug>.patch: {patch.name}")
        tree = patch.name.split("-", 1)[0]
        if tree not in SOURCE_TREES:
            raise UpdateError(f"source patch names an unknown tree: {patch.name}")
        records.append((patch.name, "." if tree == "smolvm" else tree, hashlib.sha256(patch.read_bytes()).hexdigest()))
    return records


def apply_patches(root: Path, patch_dir: Path, records: List[Tuple[str, str, str]]) -> None:
    for name, tree, _ in records:
        patch = patch_dir / name
        target = root / tree
        check = subprocess.run(
            ["patch", "-p1", "-d", str(target), "--dry-run", "--batch", "--forward", "-i", str(patch)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if check.returncode != 0:
            detail = check.stderr.strip() or check.stdout.strip()
            raise UpdateError(f"source patch conflict: {name}: {detail}")
        applied = subprocess.run(
            ["patch", "-p1", "-d", str(target), "--batch", "--forward", "-i", str(patch)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if applied.returncode != 0:
            detail = applied.stderr.strip() or applied.stdout.strip()
            raise UpdateError(f"cannot apply source patch: {name}: {detail}")


LIBKRUN_FEATURES = "blk,net,gpu,input"
TOOLCHAIN_INIT = "nightly-2026-09-15"
TOOLCHAIN_GUEST = "1.99.1"


def toml_string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def lock_bytes(fields: Dict[str, object]) -> bytes:
    lines: List[str] = []
    sections = [name for name in fields if name != "patch"]
    for section in sections:
        values = fields[section]
        assert isinstance(values, dict)
        lines.append(f"[{section}]")
        for key, value in values.items():
            if isinstance(value, list):
                lines.append(f"{key} = [{', '.join(toml_string(item) for item in value)}]")
            elif isinstance(value, int):
                lines.append(f"{key} = {value}")
            else:
                lines.append(f"{key} = {toml_string(str(value))}")
        lines.append("")
    for values in fields.get("patch", []):  # type: ignore[union-attr]
        lines.append("[[patch]]")
        for key, value in values.items():
            lines.append(f"{key} = {toml_string(str(value))}")
        lines.append("")
    return ("\n".join(lines).rstrip("\n") + "\n").encode()


def read_lock(lock_path: Path) -> Dict[str, object]:
    try:
        return tomllib.loads(lock_path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise UpdateError(f"invalid source lock: {error}") from error


def existing_source(third_party: Path, lock_path: Path) -> bool:
    target = third_party / "smolvm"
    target_exists = target.exists()
    lock_exists = lock_path.exists()
    if target_exists != lock_exists:
        raise UpdateError("existing vendored source and UPSTREAM.lock are incomplete")
    if not target_exists:
        return False
    if target.is_symlink() or not target.is_dir() or lock_path.is_symlink():
        raise UpdateError("refusing to replace a symlink or unexpected vendor layout")
    expected = read_lock(lock_path).get("import", {}).get("source_identity", "")
    actual, _ = source_identity(target)
    if not expected or actual != expected:
        raise UpdateError("existing vendored source differs from UPSTREAM.lock")
    return True


def source_diff(old: Optional[Path], new: Path) -> None:
    new_id, new_entries = source_identity(new)
    if old is None:
        print(f"source diff: initial import, {len(new_entries)} entries")
    else:
        old_id, old_entries = source_identity(old)
        added = sorted(new_entries.keys() - old_entries.keys())
        removed = sorted(old_entries.keys() - new_entries.keys())
        changed = sorted(
            path for path in new_entries.keys() & old_entries.keys() if new_entries[path] != old_entries[path]
        )
        print(f"source diff: {len(added)} added, {len(removed)} removed, {len(changed)} changed")
        for prefix, paths in (("+", added), ("-", removed), ("~", changed)):
            for path in paths[:40]:
                print(f"  {prefix} {path}")
            if len(paths) > 40:
                print(f"  ... {len(paths) - 40} more {prefix} entries")
        print(f"source identity: {old_id} -> {new_id}")
    print(f"source identity: {new_id}")


def publish(work: Path, target: Path, lock_path: Path, staged_source: Path, staged_lock: Path, had_source: bool) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    if os.stat(work).st_dev != os.stat(target.parent).st_dev:
        raise UpdateError("staging and vendor directories are on different filesystems")
    backup_source = work / "previous-smolvm"
    backup_lock = work / "previous-UPSTREAM.lock"
    moved_source = False
    moved_lock = False
    installed_source = False
    try:
        if had_source:
            os.replace(target, backup_source)
            moved_source = True
            os.replace(lock_path, backup_lock)
            moved_lock = True
        os.replace(staged_source, target)
        installed_source = True
        os.replace(staged_lock, lock_path)
    except Exception:
        if installed_source and target.exists():
            shutil.rmtree(target)
        if moved_source and backup_source.exists():
            os.replace(backup_source, target)
        if moved_lock and backup_lock.exists():
            os.replace(backup_lock, lock_path)
        raise


def fetch_locked_kernel(repo_root: Path, cache_dir: Path) -> Path:
    repo_root = require_repo_root(repo_root)
    require_ignored_work(repo_root)
    work_path = repo_root / ".work"
    if work_path.is_symlink():
        raise UpdateError("refusing a symlinked .work directory")
    work_path.mkdir(parents=True, exist_ok=True)
    work = work_path.resolve(strict=True)
    if not cache_dir.is_absolute():
        cache_dir = repo_root / cache_dir
    lexical_cache = Path(os.path.abspath(cache_dir))
    try:
        lexical_cache.relative_to(work_path)
    except ValueError as error:
        raise UpdateError("kernel cache must remain under .work") from error
    current = work_path
    for component in lexical_cache.relative_to(work_path).parts:
        current = current / component
        if current.is_symlink():
            raise UpdateError(f"refusing a symlinked kernel cache path: {current}")
    cache_dir = cache_dir.resolve(strict=False)
    if os.path.commonpath((str(work), str(cache_dir))) != str(work):
        raise UpdateError("kernel cache must remain under .work")
    third_party = repo_root / "third_party"
    lock_path = third_party / "UPSTREAM.lock"
    if not existing_source(third_party, lock_path):
        raise UpdateError("vendored SmolVM source is not installed")
    kernel_name, _ = kernel_version(third_party / "smolvm" / "libkrunfw")
    filename = kernel_name + ".tar.xz"
    locked = read_lock(lock_path).get("kernel")
    if not isinstance(locked, dict):
        raise UpdateError("UPSTREAM.lock has no pinned kernel archive")
    try:
        url = locked["url"]
        revision = locked["revision"]
        archive_digest = locked["sha256"]
    except KeyError as error:
        raise UpdateError(f"UPSTREAM.lock has no pinned kernel archive: {error}") from error
    if url != kernel_archive_url(kernel_name.removeprefix("linux-")):
        raise UpdateError("UPSTREAM.lock kernel URL does not match the pinned kernel version")
    if not isinstance(revision, str) or not HEX_40.fullmatch(revision):
        raise UpdateError("UPSTREAM.lock kernel revision is invalid")
    if not isinstance(archive_digest, str) or not HEX_64.fullmatch(archive_digest):
        raise UpdateError("UPSTREAM.lock kernel archive digest is invalid")
    return fetch_kernel_cache(cache_dir, filename, str(url), archive_digest, kernel_name)


def download_kernel_archive(
    url: str, cache_dir: Path, filename: str, archive_digest: str, kernel_name: str
) -> Path:
    if not HEX_64.fullmatch(archive_digest) or Path(filename).name != filename:
        raise UpdateError("kernel source lock contains an invalid archive identity")
    if cache_dir.is_symlink():
        raise UpdateError(f"refusing a symlinked kernel cache directory: {cache_dir}")
    cache_dir.mkdir(parents=True, exist_ok=True)
    if not cache_dir.is_dir():
        raise UpdateError(f"refusing an unsafe kernel cache directory: {cache_dir}")
    target = cache_dir / filename
    if target.is_symlink():
        raise UpdateError(f"refusing to replace a symlinked kernel archive: {target}")
    output_path = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb", prefix=f".{filename}.", suffix=".partial", dir=cache_dir, delete=False
        ) as output:
            output_path = Path(output.name)
        download(url, output_path)
        if file_sha256(output_path) != archive_digest:
            raise UpdateError(f"kernel source checksum mismatch for {filename}")
        validate_kernel_archive(output_path, kernel_name)
        with output_path.open("rb") as output:
            os.fsync(output.fileno())
        os.replace(output_path, target)
    except Exception:
        if output_path is not None:
            output_path.unlink(missing_ok=True)
        raise
    return target


def fetch_kernel_cache(
    cache_dir: Path, filename: str, url: str, archive_digest: str, kernel_name: str
) -> Path:
    if cache_dir.is_symlink():
        raise UpdateError(f"refusing a symlinked kernel cache directory: {cache_dir}")
    cache_dir.mkdir(parents=True, exist_ok=True)
    if not cache_dir.is_dir():
        raise UpdateError(f"refusing an unsafe kernel cache directory: {cache_dir}")
    target = cache_dir / filename
    if target.is_symlink():
        raise UpdateError(f"refusing a symlinked kernel cache entry: {target}")
    if target.exists():
        if not target.is_file() or file_sha256(target) != archive_digest:
            raise UpdateError(f"cached kernel source checksum mismatch: {target}")
        validate_kernel_archive(target, kernel_name)
        return target
    return download_kernel_archive(url, cache_dir, filename, archive_digest, kernel_name)


def verify_source(repo_root: Path) -> str:
    third_party = repo_root / "third_party"
    if not existing_source(third_party, third_party / "UPSTREAM.lock"):
        raise UpdateError("no vendored source or UPSTREAM.lock to verify")
    return source_identity(third_party / "smolvm")[0]


def import_source(args: argparse.Namespace) -> None:
    repo_root = require_repo_root(args.repo_root)
    require_ignored_work(repo_root)
    third_party = repo_root / "third_party"
    target_path = third_party / "smolvm"
    lock_path = third_party / "UPSTREAM.lock"
    had_source = existing_source(third_party, lock_path)
    if args.release:
        release_parts(args.release)
        source_target = SourceTarget(
            label=args.release, revision=args.release, commit=None, branch=None, is_release=True
        )
    else:
        source_target = default_branch_tip()
    source_root = args.source_root.resolve(strict=True) if args.source_root else None
    work = repo_root / ".work" / "tmp"
    work.mkdir(parents=True, exist_ok=True)
    os.environ["TMPDIR"] = str(work)
    with tempfile.TemporaryDirectory(prefix="update-smolvm-", dir=work) as temporary:
        temporary_path = Path(temporary)
        stage_root = temporary_path / "smolvm"
        smolvm, components, links, urls = source_repositories(source_target, source_root, temporary_path)
        extract_git_archive(smolvm, stage_root, "smolvm")
        for name in ("libkrun", "libkrunfw"):
            extract_git_archive(components[name], stage_root / name, name)
        kernel_name, version = kernel_version(stage_root / "libkrunfw")
        filename = kernel_name + ".tar.xz"
        kernel_url = kernel_archive_url(version)
        checksums_url = kernel_url.rsplit("/", 1)[0] + "/sha256sums.asc"
        checksums_path = temporary_path / "kernel-checksums.asc"
        download(checksums_url, checksums_path)
        kernel_digest = checksum_for(checksums_path.read_text(), filename)
        if not HEX_64.fullmatch(kernel_digest):
            raise UpdateError(f"invalid published kernel checksum for {filename}")
        kernel_revision = kernel_revision_for(version)
        version_string = package_version(stage_root)
        records = patch_records(repo_root / "runtime" / "patches")
        apply_patches(stage_root, repo_root / "runtime" / "patches", records)
        validate_layout(stage_root)
        identity, _ = source_identity(stage_root)
        source_diff(target_path if had_source else None, stage_root)
        fields: Dict[str, object] = {
            "import": {
                "format": 1,
                "target": source_target.label,
                "source_identity": identity,
                "components": f"smolvm,libkrun,libkrunfw,{kernel_name}",
                "patches": [name for name, _, _ in records],
            },
            "smolvm": {
                "url": SMOLVM_URL,
                "revision": head_revision(smolvm),
                "version": version_string,
            },
            "libkrun": {
                "url": urls["libkrun"],
                "revision": links["libkrun"],
                "features": LIBKRUN_FEATURES,
            },
            "libkrunfw": {"url": urls["libkrunfw"], "revision": links["libkrunfw"]},
            "kernel": {
                "url": kernel_url,
                "version": kernel_name,
                "revision": kernel_revision,
                "sha256": kernel_digest,
            },
            "toolchain": {"init": TOOLCHAIN_INIT, "guest": TOOLCHAIN_GUEST},
        }
        if source_target.branch:
            fields["smolvm"]["branch"] = source_target.branch
        patches = []
        for name, tree, digest in records:
            patches.append({"path": name, "tree": tree, "sha256": digest})
        if patches:
            fields["patch"] = patches
        staged_lock = temporary_path / "UPSTREAM.lock"
        staged_lock.write_bytes(lock_bytes(fields))
        publish(temporary_path, target_path, lock_path, stage_root, staged_lock, had_source)
    print(f"updated third_party/smolvm from {source_target.label} ({identity})")


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Vendor SmolVM, libkrun, and libkrunfw into third_party.")
    parser.add_argument("release", nargs="?", help="a stable vX.Y.Z tag to pin instead of the default-branch tip")
    parser.add_argument("--source-root", type=Path, help="offline source root holding smolvm/, libkrun/, libkrunfw/")
    parser.add_argument("--fetch-kernel", type=Path, help="verify the pinned kernel archive into this cache directory")
    parser.add_argument("--verify-source", action="store_true", help="check the vendored source against UPSTREAM.lock and exit")
    parser.add_argument("--repo-root", type=Path, required=True)
    args = parser.parse_args(argv)
    if args.verify_source and (args.release or args.source_root or args.fetch_kernel):
        parser.error("--verify-source cannot be combined with a source update or kernel fetch")
    if args.source_root and not args.release:
        parser.error("--source-root requires a release tag")
    if args.fetch_kernel and (args.release or args.source_root):
        parser.error("--fetch-kernel cannot be combined with a source update")
    return args


def main(argv: Optional[Sequence[str]] = None) -> int:
    try:
        args = parse_args(argv)
        if args.verify_source:
            print(f"vendored source matches UPSTREAM.lock ({verify_source(args.repo_root)})")
        elif args.fetch_kernel:
            print(f"verified kernel source cached at {fetch_locked_kernel(args.repo_root, args.fetch_kernel)}")
        else:
            import_source(args)
    except (UpdateError, OSError, tarfile.TarError) as error:
        print(f"update-smolvm: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
