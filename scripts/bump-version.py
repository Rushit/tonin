#!/usr/bin/env python3
"""
scripts/bump-version.py — bump the workspace version.

SOURCE OF TRUTH: /VERSION
Mirrors the new version into [workspace.package].version in Cargo.toml,
updates intra-workspace dep pins in [workspace.dependencies], refreshes
Cargo.lock, and creates a clean commit.

Does NOT tag and does NOT push — those belong to the release workflow.

Usage:
    scripts/bump-version.py <X.Y.Z[-pre]>
    scripts/bump-version.py <patch|minor|major>

Works on macOS, Linux, and Windows (no bash/awk/sed required).
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def repo_root() -> Path:
    script = Path(__file__).resolve()
    return script.parent.parent


def read_version(root: Path) -> str:
    v = (root / "VERSION").read_text(encoding="utf-8").strip()
    if not v:
        sys.exit("error: VERSION file is empty")
    return v


def read_cargo_version(root: Path) -> str:
    """Read [workspace.package].version from Cargo.toml."""
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
    sys.exit("error: could not find version in [workspace.package] block of Cargo.toml")


def rewrite_workspace_package_version(text: str, new: str) -> str:
    """Replace version = "..." in [workspace.package] section."""
    lines = text.splitlines(keepends=True)
    in_block = False
    replaced = False
    result = []
    for line in lines:
        stripped = line.strip()
        if stripped == "[workspace.package]":
            in_block = True
            result.append(line)
            continue
        if stripped.startswith("[") and in_block:
            in_block = False
        if in_block and not replaced and re.match(r"^version\s*=", stripped):
            # Preserve inline comment if present
            comment_match = re.search(r'#[^"]*$', line)
            comment = f"  {comment_match.group()}" if comment_match else ""
            result.append(f'version = "{new}"{comment}\n')
            replaced = True
            continue
        result.append(line)
    if not replaced:
        sys.exit("error: could not find version line to replace in Cargo.toml")
    return "".join(result)


def rewrite_workspace_dep_pins(text: str, new: str) -> tuple[str, list[str]]:
    """
    In [workspace.dependencies], update version = "..." on every tonin* line.
    Returns (new_text, list_of_stale_lines_that_were_not_updated).
    """
    lines = text.splitlines(keepends=True)
    in_block = False
    result = []
    stale = []
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("[workspace.dependencies]"):
            in_block = True
            result.append(line)
            continue
        if stripped.startswith("[") and in_block:
            in_block = False
        if (
            in_block
            and re.match(r"^tonin[A-Za-z0-9_-]*\s*=", stripped)
            and "version" in line
        ):
            updated = re.sub(r'version\s*=\s*"[^"]+"', f'version = "{new}"', line)
            result.append(updated)
            # Check if it actually changed
            if updated == line and f'"{new}"' not in line:
                stale.append(line.rstrip())
            continue
        result.append(line)
    return "".join(result), stale


def is_dirty(root: Path) -> bool:
    r = subprocess.run(
        ["git", "diff", "--quiet"],
        cwd=root,
    )
    if r.returncode != 0:
        return True
    r = subprocess.run(
        ["git", "diff", "--cached", "--quiet"],
        cwd=root,
    )
    return r.returncode != 0


def run(cmd: list[str], *, cwd: Path) -> None:
    result = subprocess.run(cmd, cwd=cwd)
    if result.returncode != 0:
        sys.exit(result.returncode)


# ---------------------------------------------------------------------------
# Version arithmetic
# ---------------------------------------------------------------------------

SEMVER_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)(-[A-Za-z0-9.-]+)?$")


def parse_semver(v: str) -> tuple[int, int, int, str]:
    m = SEMVER_RE.match(v)
    if not m:
        sys.exit(f"error: '{v}' is not a valid SemVer string (expected X.Y.Z or X.Y.Z-pre)")
    return int(m.group(1)), int(m.group(2)), int(m.group(3)), m.group(4) or ""


def bump(current: str, kind: str) -> str:
    ma, mi, pa, _ = parse_semver(current)
    if kind == "major":
        return f"{ma + 1}.0.0"
    if kind == "minor":
        return f"{ma}.{mi + 1}.0"
    if kind == "patch":
        return f"{ma}.{mi}.{pa + 1}"
    sys.exit(f"error: unknown bump kind '{kind}'")


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    if len(sys.argv) != 2:
        print("error: expected exactly one argument", file=sys.stderr)
        print("usage: scripts/bump-version.py <X.Y.Z[-pre]>", file=sys.stderr)
        print("       scripts/bump-version.py <patch|minor|major>", file=sys.stderr)
        sys.exit(1)

    arg = sys.argv[1]
    root = repo_root()

    if not (root / "VERSION").exists():
        sys.exit("error: VERSION file not found at repo root")

    current = read_version(root)

    # Resolve bump-type aliases
    new = bump(current, arg) if arg in ("patch", "minor", "major") else arg
    if arg in ("patch", "minor", "major"):
        print(f"Auto-calculated bump → {new}")

    # Validate target version
    parse_semver(new)  # exits on invalid

    # Verify working tree is clean
    if is_dirty(root):
        print(
            "error: working tree has uncommitted changes; commit or stash them first",
            file=sys.stderr,
        )
        subprocess.run(["git", "status", "--short"], cwd=root)
        sys.exit(1)

    # Assert current VERSION and Cargo.toml were already in sync
    cargo_current = read_cargo_version(root)
    if cargo_current != current:
        sys.exit(
            f"error: VERSION ({current}) and Cargo.toml ({cargo_current}) disagree.\n"
            "       Resync by editing both to match, then re-run this script."
        )

    print(f"Current version: {current}")
    print(f"New version:     {new}")

    if new == current:
        print(f"Notice: new version equals current ({current}). Skipping.")
        sys.exit(0)

    cargo_toml = root / "Cargo.toml"
    text = cargo_toml.read_text(encoding="utf-8")

    # 1. Update [workspace.package].version
    text = rewrite_workspace_package_version(text, new)

    # 2. Update [workspace.dependencies] tonin* version pins
    text, stale = rewrite_workspace_dep_pins(text, new)
    if stale:
        print(
            f"error: some intra-workspace dep pins did not bump to {new}:",
            file=sys.stderr,
        )
        for s in stale:
            print(f"  {s}", file=sys.stderr)
        sys.exit(1)

    # Write atomically (temp + rename)
    tmp = cargo_toml.with_suffix(".toml.bak")
    tmp.write_text(text, encoding="utf-8")
    tmp.replace(cargo_toml)

    # Sanity-check the rewrite
    verify = read_cargo_version(root)
    if verify != new:
        sys.exit(f"error: Cargo.toml rewrite failed (still reads '{verify}')")

    # 3. Write VERSION
    (root / "VERSION").write_text(f"{new}\n", encoding="utf-8")

    # 4. Refresh Cargo.lock
    print("Refreshing Cargo.lock via cargo check...")
    run(["cargo", "check", "--workspace"], cwd=root)

    # 5. Commit
    run(["git", "add", "VERSION", "Cargo.toml", "Cargo.lock"], cwd=root)
    run(["git", "commit", "-m", f"chore: release v{new}"], cwd=root)

    print()
    print(f"Bumped {current} → {new} (VERSION + Cargo.toml in sync), committed.")
    print("Next: push to main — the auto-release workflow tags and publishes.")


if __name__ == "__main__":
    main()
