#!/usr/bin/env python3
"""
scripts/check-version.py — assert VERSION file and Cargo.toml are in sync.

Used by:
  - scripts/pre-commit  (via import or subprocess)
  - .github/workflows/ci.yml  (version-sync job)
  - .github/workflows/release.yml  (verify job)

Exit 0 on success, 1 on mismatch.  Prints VERSION and Cargo.toml values.
Works on macOS, Linux, and Windows without awk/sed/grep.

Optional argument: --tag <vX.Y.Z>
  Also asserts that the Git tag matches VERSION (used by release.yml verify).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


def repo_root() -> Path:
    """Return the repository root (parent of this script's directory)."""
    return Path(__file__).resolve().parent.parent


def read_version_file(root: Path) -> str:
    return (root / "VERSION").read_text(encoding="utf-8").strip()


def read_cargo_version(root: Path) -> str:
    in_block = False
    for line in (root / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped == "[workspace.package]":
            in_block = True
            continue
        if stripped.startswith("[") and in_block:
            break
        if in_block and stripped.startswith("version"):
            m = re.search(r'"([^"]+)"', stripped)
            if m:
                return m.group(1)
    raise SystemExit("error: could not find version in Cargo.toml [workspace.package]")


def main() -> None:
    root = repo_root()

    tag_version: str | None = None
    args = sys.argv[1:]
    if args and args[0] == "--tag" and len(args) >= 2:
        ref = args[1]          # e.g. "v0.13.2" or "0.13.2"
        tag_version = ref.lstrip("v")

    file_ver = read_version_file(root)
    cargo_ver = read_cargo_version(root)

    print(f"VERSION file : {file_ver}")
    print(f"Cargo.toml   : {cargo_ver}")
    if tag_version:
        print(f"Git tag      : {tag_version}")

    ok = True
    if file_ver != cargo_ver:
        print(
            f"error: VERSION ({file_ver}) and Cargo.toml ({cargo_ver}) are out of sync.",
            file=sys.stderr,
        )
        print(
            f"error: Run  scripts/bump-version.py {cargo_ver}  to fix both at once.",
            file=sys.stderr,
        )
        ok = False

    if tag_version and tag_version != file_ver:
        print(
            f"error: Git tag ({tag_version}) does not match VERSION ({file_ver}).",
            file=sys.stderr,
        )
        ok = False

    if not ok:
        sys.exit(1)

    print("check-version: ok")


if __name__ == "__main__":
    main()
