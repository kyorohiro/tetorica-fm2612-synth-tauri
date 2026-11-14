"""Prepare transactional Tauri app version changes without resolving dependencies.

The Web ZIP version is independent from this app version. All inputs and the
complete proposed update are validated before any file is written.
"""
# SPDX-License-Identifier: BSD-3-Clause

from __future__ import annotations

import copy
import json
import os
import re
import stat
import tempfile
import tomllib
from pathlib import Path
from typing import Any


FILES = (
    "package.json",
    "package-lock.json",
    "src-tauri/Cargo.toml",
    "src-tauri/tauri.conf.json",
    "src-tauri/Cargo.lock",
)
_VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")
_VERSION_LINE = re.compile(
    r"^([ \t]*version[ \t]*=[ \t]*)(?:\"[^\"\r\n]*\"|'[^'\r\n]*')"
    r"([ \t]*(?:#[^\r\n]*)?)(\r?)$",
    re.MULTILINE,
)


def validate_version(version: str) -> str:
    """Return a numeric X.Y.Z version, or raise ValueError."""
    if not isinstance(version, str) or _VERSION.fullmatch(version) is None:
        raise ValueError("Version must be numeric X.Y.Z without leading zeroes.")
    return version


def _load(repo: Path) -> dict[str, str]:
    texts = {}
    for filename in FILES:
        path = Path(repo) / filename
        try:
            if path.is_symlink() or not path.is_file():
                raise ValueError(f"Expected a regular metadata file: {path}")
            texts[filename] = path.read_bytes().decode("utf-8")
        except OSError as error:
            raise ValueError(f"Cannot read {path}: {error.strerror}") from error
    return texts


def _parse(texts: dict[str, str]) -> dict[str, Any]:
    parsed = {}
    for filename, text in texts.items():
        try:
            parsed[filename] = (
                json.loads(text) if filename.endswith(".json") else tomllib.loads(text)
            )
        except (ValueError, tomllib.TOMLDecodeError) as error:
            raise ValueError(f"Invalid metadata in {filename}: {error}") from error
        if not isinstance(parsed[filename], dict):
            raise ValueError(f"{filename} must contain an object/table.")
    return parsed


def _field(obj: Any, key: str, location: str) -> Any:
    if not isinstance(obj, dict) or key not in obj:
        raise ValueError(f"Missing {location}.{key}")
    return obj[key]


def _string(obj: Any, key: str, location: str) -> str:
    value = _field(obj, key, location)
    if not isinstance(value, str) or not value:
        raise ValueError(f"{location}.{key} must be a nonempty string.")
    return value


def _own_package(data: dict[str, Any], name: str) -> dict[str, Any]:
    packages = _field(data["src-tauri/Cargo.lock"], "package", "Cargo.lock")
    if not isinstance(packages, list) or not all(isinstance(p, dict) for p in packages):
        raise ValueError("Cargo.lock.package must be a list of tables.")
    own = [p for p in packages if p.get("name") == name and "source" not in p]
    if len(own) != 1:
        raise ValueError(f"Cargo.lock must contain exactly one local package {name!r}.")
    return own[0]


def _metadata(data: dict[str, Any]) -> dict[str, str]:
    package = data["package.json"]
    lock = data["package-lock.json"]
    cargo = _field(data["src-tauri/Cargo.toml"], "package", "Cargo.toml")
    config = data["src-tauri/tauri.conf.json"]
    cargo_name = _string(cargo, "name", "Cargo.toml.package")
    npm_root = _field(_field(lock, "packages", "package-lock.json"), "", "package-lock.json.packages")
    own = _own_package(data, cargo_name)
    result = {
        "name": cargo_name,
        "product": _string(config, "productName", "tauri.conf.json"),
        "packageName": _string(package, "name", "package.json"),
        "packageVersion": _string(package, "version", "package.json"),
        "packageLockVersion": _string(lock, "version", "package-lock.json"),
        "packageLockRootVersion": _string(npm_root, "version", "package-lock.json.packages['']"),
        "cargoName": cargo_name,
        "cargoVersion": _string(cargo, "version", "Cargo.toml.package"),
        "configVersion": _string(config, "version", "tauri.conf.json"),
        "cargoLockVersion": _string(own, "version", "Cargo.lock local package"),
    }
    for key, value in result.items():
        if key.endswith("Version"):
            validate_version(value)
    result["version"] = result["packageVersion"]
    return result


def read_metadata(repo: Path) -> dict[str, str]:
    """Read numeric app versions, allowing existing differences to be repaired."""
    return _metadata(_parse(_load(repo)))


def _dependencies(data: dict[str, Any], name: str) -> list[dict[str, Any]]:
    own = _own_package(data, name)
    return copy.deepcopy([p for p in data["src-tauri/Cargo.lock"]["package"] if p is not own])


def dependency_snapshot(repo: Path) -> list[dict[str, Any]]:
    """Return every foreign Cargo.lock package, including all dependency fields."""
    data = _parse(_load(repo))
    metadata = _metadata(data)
    return _dependencies(data, metadata["name"])


def _replace_version(section: str, version: str, location: str) -> str:
    replaced, count = _VERSION_LINE.subn(
        lambda match: f'{match[1]}"{version}"{match[2]}{match[3]}', section
    )
    if count != 1:
        raise ValueError(f"{location} must contain one literal version assignment.")
    return replaced


def _sections(text: str, header: str) -> list[tuple[int, int]]:
    starts = list(re.finditer(rf"(?m)^[ \t]*{re.escape(header)}[ \t]*(?:#[^\r\n]*)?(?:\r?\n|$)", text))
    spans = []
    for start in starts:
        following = re.search(r"(?m)^[ \t]*\[", text[start.end():])
        end = start.end() + following.start() if following else len(text)
        spans.append((start.start(), end))
    return spans


def _cargo_version(text: str, version: str) -> str:
    sections = _sections(text, "[package]")
    if len(sections) != 1:
        raise ValueError("Cargo.toml must contain exactly one [package] table.")
    start, end = sections[0]
    return text[:start] + _replace_version(text[start:end], version, "Cargo.toml [package]") + text[end:]


def _cargo_lock_version(text: str, name: str, version: str) -> str:
    own = []
    for start, end in _sections(text, "[[package]]"):
        package = tomllib.loads(text[start:end])["package"][0]
        if package.get("name") == name and "source" not in package:
            own.append((start, end))
    if len(own) != 1:
        raise ValueError(f"Cannot identify the local Cargo.lock table for {name!r}.")
    start, end = own[0]
    return text[:start] + _replace_version(text[start:end], version, "Cargo.lock local package") + text[end:]


def _verify(metadata: dict[str, str], version: str) -> None:
    for key in (
        "packageVersion", "packageLockVersion", "packageLockRootVersion",
        "cargoVersion", "configVersion", "cargoLockVersion",
    ):
        if metadata[key] != version:
            raise ValueError(f"{key} is {metadata[key]}, expected {version}.")


def verify_metadata(repo: Path, version: str) -> None:
    """Require all app metadata and both lock files to match the target."""
    _verify(read_metadata(repo), validate_version(version))


class VersionUpdate:
    """A prepared, optimistic transaction over the five local metadata files."""

    def __init__(self, root, version, originals, proposed, modes):
        self.root = root
        self.version = version
        self._originals = originals
        self._proposed = proposed
        self._modes = modes

    def _unchanged(self, filename):
        path = self.root / filename
        if (path.is_symlink() or not path.is_file()
                or path.read_bytes() != self._originals[filename]
                or stat.S_IMODE(path.stat().st_mode) != self._modes[filename]):
            raise ValueError(f"Metadata changed after version preparation: {filename}")

    def apply(self):
        """Apply staged versions, restoring replaced metadata if a write fails."""
        for filename in FILES:
            self._unchanged(filename)
        changed = [name for name in FILES if self._proposed[name] != self._originals[name]]
        if not changed:
            return
        with tempfile.TemporaryDirectory(prefix=".app-version-", dir=self.root) as temporary:
            staged = Path(temporary)
            # Stage every replacement and its original before touching targets.
            for index, filename in enumerate(changed):
                for suffix, content in (("new", self._proposed[filename]), ("old", self._originals[filename])):
                    candidate = staged / f"{index}.{suffix}"
                    candidate.write_bytes(content)
                    candidate.chmod(self._modes[filename])
            for filename in FILES:
                self._unchanged(filename)
            replaced = []
            try:
                for index, filename in enumerate(changed):
                    self._unchanged(filename)
                    os.replace(staged / f"{index}.new", self.root / filename)
                    replaced.append((index, filename))
            except BaseException as error:
                failures = []
                for index, filename in reversed(replaced):
                    try:
                        os.replace(staged / f"{index}.old", self.root / filename)
                    except OSError:
                        failures.append(filename)
                if failures:
                    raise RuntimeError("Version update rollback failed for: " + ", ".join(failures)) from error
                raise


def prepare_version_update(repo: Path, version: str) -> VersionUpdate:
    """Validate and propose all five files without writing or running commands."""
    version = validate_version(version)
    repo = Path(repo)
    texts = _load(repo)
    originals = {filename: (repo / filename).read_bytes() for filename in FILES}
    if any(originals[name].decode("utf-8") != texts[name] for name in FILES):
        raise ValueError("Metadata changed while preparing the version update.")
    modes = {filename: stat.S_IMODE((repo / filename).stat().st_mode) for filename in FILES}
    data = _parse(texts)
    metadata = _metadata(data)
    dependencies = _dependencies(data, metadata["name"])
    proposed = dict(texts)
    data["package.json"]["version"] = version
    data["package-lock.json"]["version"] = version
    data["package-lock.json"]["packages"][""]["version"] = version
    data["src-tauri/tauri.conf.json"]["version"] = version
    for filename in ("package.json", "package-lock.json", "src-tauri/tauri.conf.json"):
        proposed[filename] = json.dumps(data[filename], ensure_ascii=False, indent=2) + "\n"
    proposed["src-tauri/Cargo.toml"] = _cargo_version(texts["src-tauri/Cargo.toml"], version)
    proposed["src-tauri/Cargo.lock"] = _cargo_lock_version(texts["src-tauri/Cargo.lock"], metadata["name"], version)
    checked = _parse(proposed)
    _verify(_metadata(checked), version)
    if _dependencies(checked, metadata["name"]) != dependencies:
        raise ValueError("App version update unexpectedly changed Cargo dependencies.")
    encoded = {}
    for filename in FILES:
        # Preserve exact original bytes for untouched metadata and its rollback.
        encoded[filename] = (originals[filename] if texts[filename] == proposed[filename]
                             else proposed[filename].encode("utf-8"))
    return VersionUpdate(repo, version, originals, encoded, modes)
