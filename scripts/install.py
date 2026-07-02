#!/usr/bin/env python3
"""
scripts/install.py — cross-platform installer for the tonin CLI.

Works natively on macOS, Linux, and Windows (PowerShell / cmd / py.exe).
No bash, curl, tar, or unzip required — pure Python 3 stdlib.

Usage (any platform):
    python3 scripts/install.py
    python3 scripts/install.py --version v0.5.4
    python3 scripts/install.py --dir C:\\Users\\me\\bin
    python3 scripts/install.py --plugin owner/tonin-myplugin
    python3 scripts/install.py --plugin owner/tonin-myplugin@v1.2.3

Windows one-liner (PowerShell):
    py -3 -c "import urllib.request; exec(urllib.request.urlopen('https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.py').read())"

macOS / Linux one-liner:
    curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.py | python3
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

REPO_TONIN = "Rushit/tonin"

# ---------------------------------------------------------------------------
# Colour helpers (disabled on Windows unless ANSI is supported)
# ---------------------------------------------------------------------------
_ANSI = sys.stdout.isatty() and (
    platform.system() != "Windows"
    or os.environ.get("TERM_PROGRAM") == "vscode"
    or "WT_SESSION" in os.environ          # Windows Terminal
    or os.environ.get("COLORTERM")
)


def _c(code: str, text: str) -> str:
    return f"\033[{code}m{text}\033[0m" if _ANSI else text


def say(msg: str) -> None:
    print(_c("1", msg))


def ok(msg: str) -> None:
    print(_c("32", "✓") + " " + msg)


def warn(msg: str) -> None:
    print(_c("33", "Note:") + " " + msg)


def err(msg: str, code: int = 1) -> None:
    print(_c("31", "error:") + " " + msg, file=sys.stderr)
    sys.exit(code)


# ---------------------------------------------------------------------------
# OS / arch detection → Rust target triple
# ---------------------------------------------------------------------------

def detect_target() -> str:
    system = platform.system()
    machine = platform.machine().lower()

    if system == "Linux":
        arch = "x86_64" if machine in ("x86_64", "amd64") else \
               "aarch64" if machine in ("aarch64", "arm64") else None
        if not arch:
            err(
                f"Unsupported Linux architecture: {machine} "
                "(pre-built binary not available; use 'cargo install tonin')"
            )
        return f"{arch}-unknown-linux-musl"

    if system == "Darwin":
        arch = "x86_64" if machine == "x86_64" else \
               "aarch64" if machine in ("arm64", "aarch64") else None
        if not arch:
            err(f"Unsupported macOS architecture: {machine}")
        return f"{arch}-apple-darwin"

    if system == "Windows":
        arch = "x86_64" if machine in ("amd64", "x86_64") else \
               "aarch64" if machine in ("arm64", "aarch64") else None
        if not arch:
            err(f"Unsupported Windows architecture: {machine}")
        return f"{arch}-pc-windows-msvc"

    err(
        f"Unsupported OS: {system} "
        "(pre-built binary not available; use 'cargo install tonin')"
    )


# ---------------------------------------------------------------------------
# Default install directory
# ---------------------------------------------------------------------------

def default_install_dir() -> Path:
    # Same as cargo install: ~/.cargo/bin — already on PATH for Rust devs.
    cargo_bin = Path.home() / ".cargo" / "bin"
    return cargo_bin


# ---------------------------------------------------------------------------
# GitHub API helpers
# ---------------------------------------------------------------------------

def _gh_get(url: str) -> dict:
    req = urllib.request.Request(
        url,
        headers={"Accept": "application/vnd.github+json",
                 "User-Agent": f"tonin-installer/python ({sys.platform})"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        err(f"GitHub API request failed ({e.code}): {url}")
    except OSError as e:
        err(f"Network error: {e}")


def latest_tag(repo: str) -> str:
    data = _gh_get(f"https://api.github.com/repos/{repo}/releases/latest")
    tag = data.get("tag_name", "")
    if not tag:
        err("Could not determine latest version. Pass --version vX.Y.Z explicitly.")
    return tag


# ---------------------------------------------------------------------------
# Installed-version probe
# ---------------------------------------------------------------------------

def installed_version(bin_name: str, dest_dir: Path) -> str:
    suffix = ".exe" if platform.system() == "Windows" else ""
    dest = dest_dir / (bin_name + suffix)
    if not dest.is_file():
        return ""
    try:
        out = subprocess.check_output([str(dest), "--version"],
                                      stderr=subprocess.DEVNULL,
                                      timeout=5).decode().strip()
        # clap emits "<bin> X.Y.Z" — take the last word
        return out.split()[-1] if out else ""
    except Exception:
        return ""


# ---------------------------------------------------------------------------
# Download helper
# ---------------------------------------------------------------------------

def _download(url: str, dest: Path) -> None:
    say(f"  Downloading {url}")
    try:
        with urllib.request.urlopen(
            urllib.request.Request(
                url,
                headers={"User-Agent": f"tonin-installer/python ({sys.platform})"},
            ),
            timeout=120,
        ) as resp, open(dest, "wb") as f:
            shutil.copyfileobj(resp, f)
    except urllib.error.HTTPError as e:
        err(f"Download failed ({e.code}). Check that the version exists.")
    except OSError as e:
        err(f"Download error: {e}")


# ---------------------------------------------------------------------------
# Install one binary
# ---------------------------------------------------------------------------

def install_binary(
    repo: str,
    bin_name: str,
    version: str,
    target: str,
    dest_dir: Path,
) -> None:
    is_windows = target.endswith("windows-msvc")
    ext = "zip" if is_windows else "tar.gz"
    bin_filename = bin_name + (".exe" if is_windows else "")
    dest = dest_dir / bin_filename

    # Check currently installed version
    current = installed_version(bin_name, dest_dir)
    want = version.lstrip("v")

    if current and current == want:
        ok(f"{bin_name} {current} is already up to date.")
        return

    if current:
        say(f"Upgrading {bin_name}: v{current} → {version}...")
    else:
        say(f"Installing {bin_name} {version}...")

    archive_name = f"{bin_name}-{target}.{ext}"
    url = f"https://github.com/{repo}/releases/download/{version}/{archive_name}"

    with tempfile.TemporaryDirectory() as tmp_str:
        tmp = Path(tmp_str)
        archive = tmp / f"archive.{ext}"

        _download(url, archive)

        # Extract
        if ext == "tar.gz":
            with tarfile.open(archive, "r:gz") as tf:
                tf.extractall(tmp)
        else:
            with zipfile.ZipFile(archive) as zf:
                zf.extractall(tmp)

        # Find the binary inside the extracted tree
        matches = list(tmp.rglob(bin_filename))
        if not matches:
            err(f"Binary '{bin_filename}' not found in archive.")
        bin_path = matches[0]

        # Make executable (no-op on Windows)
        bin_path.chmod(bin_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

        # Atomic install (stage + rename) — avoids ETXTBSY on self-upgrade
        staged = dest.with_suffix(f".new.{os.getpid()}" + (".exe" if is_windows else ""))
        try:
            shutil.copy2(bin_path, staged)
            staged.chmod(staged.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
            staged.replace(dest)
        except PermissionError:
            # Try with elevation hint on Windows; fall back to advice on Unix
            if platform.system() == "Windows":
                err(
                    f"Permission denied writing to {dest_dir}.\n"
                    "       Re-run this script from an elevated PowerShell prompt,\n"
                    f"       or choose a writable --dir (e.g. --dir %USERPROFILE%\\.cargo\\bin)."
                )
            else:
                err(
                    f"Permission denied writing to {dest_dir}.\n"
                    f"       Try: sudo python3 {Path(__file__).name} --dir {dest_dir}\n"
                    f"       or choose a writable --dir (e.g. --dir ~/.local/bin)."
                )

    if current:
        ok(f"Upgraded {bin_name}: v{current} → {version} ({dest})")
    else:
        ok(f"Installed {bin_name} {version} → {dest}")


# ---------------------------------------------------------------------------
# PATH check
# ---------------------------------------------------------------------------

def check_path(dest_dir: Path) -> None:
    path_dirs = [Path(p) for p in os.environ.get("PATH", "").split(os.pathsep) if p]
    try:
        resolved = dest_dir.resolve()
        if resolved not in [p.resolve() for p in path_dirs]:
            warn(f"{dest_dir} is not on your PATH.")
            if platform.system() == "Windows":
                print(f"      Add it in System Properties → Environment Variables,")
                print(f"      or run:  $env:PATH += \";{dest_dir}\"  in PowerShell.")
            else:
                print(f"      Add this to your shell profile:")
                print(f'        export PATH="{dest_dir}:$PATH"')
            print()
    except OSError:
        pass


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Install the tonin CLI (and optional plugins).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    p.add_argument("--version", default="", help="tonin version to install (default: latest)")
    p.add_argument("--dir", default="", dest="install_dir", help="install directory (default: ~/.cargo/bin)")
    p.add_argument("--plugin", action="append", default=[], dest="plugins",
                   metavar="owner/repo[@vX.Y.Z]",
                   help="install an additional plugin (repeatable)")
    return p.parse_args()


def main() -> None:
    args = parse_args()

    target = detect_target()
    dest_dir = Path(args.install_dir) if args.install_dir else default_install_dir()
    dest_dir.mkdir(parents=True, exist_ok=True)

    say(f"Target:      {target}")
    say(f"Install dir: {dest_dir}")
    print()

    # Resolve tonin version
    version = args.version
    if not version:
        say("Fetching latest tonin version...")
        version = latest_tag(REPO_TONIN)

    install_binary(REPO_TONIN, "tonin", version, target, dest_dir)

    # Extra plugins
    installed_plugins: list[str] = []
    for spec in args.plugins:
        repo, _, pin = spec.partition("@")
        bin_name = repo.split("/")[-1]
        plug_version = pin if pin else ""
        print()
        if not plug_version:
            say(f"Fetching latest {bin_name} version...")
            plug_version = latest_tag(repo)
        install_binary(repo, bin_name, plug_version, target, dest_dir)
        installed_plugins.append(bin_name)

    print()
    check_path(dest_dir)

    ok("Done! Run 'tonin --version' to verify.")
    ok("Helm chart generation is built in: 'tonin helm generate --help'")
    for bin_name in installed_plugins:
        name = bin_name.removeprefix("tonin-")
        ok(f"Run 'tonin {name} --tonin-describe' to verify {bin_name}.")


if __name__ == "__main__":
    main()
