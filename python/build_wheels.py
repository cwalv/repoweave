#!/usr/bin/env python3
"""
Build platform-specific Python wheels that bundle the rwv binaries.

Every binary the release ships — `rwv` and each bundled plugin, the list
`[dist.binaries]` in dist-workspace.toml declares — is taken from a cargo-dist
release archive and placed in the wheel's "data/scripts" directory, the layout
ruff and similar native-binary Python distributions use:

    repoweave-{version}.data/scripts/rwv        <- lands in PATH
    repoweave-{version}.data/scripts/rwv-each   <- beside it
    repoweave-{version}.dist-info/METADATA
    repoweave-{version}.dist-info/WHEEL
    repoweave-{version}.dist-info/RECORD

No Python source is included — pip/uv simply install the binary.

Usage
-----
    python build_wheels.py --version 0.2.0 \
        --archive repoweave-x86_64-unknown-linux-gnu.tar.xz \
        --platform manylinux_2_17_x86_64.manylinux2014_x86_64 --out dist/

The --platform value must be a valid wheel platform tag, e.g.
    manylinux_2_17_x86_64.manylinux2014_x86_64
    manylinux_2_17_aarch64.manylinux2014_aarch64
    macosx_11_0_arm64
    macosx_10_12_x86_64
    win_amd64

Output: dist/repoweave-{version}-py3-none-{platform}.whl
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import os
import stat
import sys
import tarfile
import tomllib
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent


# ---------------------------------------------------------------------------
# Wheel file content generators
# ---------------------------------------------------------------------------

METADATA_TEMPLATE = """\
Metadata-Version: 2.1
Name: repoweave
Version: {version}
Summary: A cross-repo workspace manager
Home-page: https://github.com/cwalv/repoweave
License: MIT
Keywords: workspace,monorepo,git,multi-repo
Classifier: Development Status :: 3 - Alpha
Classifier: Intended Audience :: Developers
Classifier: License :: OSI Approved :: MIT License
Classifier: Programming Language :: Rust
Classifier: Topic :: Software Development :: Build Tools
Requires-Python: >=3.8
"""

WHEEL_TEMPLATE = """\
Wheel-Version: 1.0
Generator: build_wheels.py
Root-Is-Purelib: false
Tag: py3-none-{platform}
"""


def _sha256_record(data: bytes) -> str:
    """Return the ``sha256=<urlsafe-b64>`` hash used in RECORD entries."""
    digest = hashlib.sha256(data).digest()
    return "sha256=" + base64.urlsafe_b64encode(digest).rstrip(b"=").decode()


def bundled_binaries(repo_root: Path = REPO_ROOT) -> list[str]:
    """The binaries every release channel ships, from dist-workspace.toml."""
    with open(repo_root / "dist-workspace.toml", "rb") as f:
        config = tomllib.load(f)
    names = config["dist"]["binaries"]["*"]
    if not names:
        raise ValueError("dist-workspace.toml [dist.binaries] names no binary")
    return list(names)


def exe_suffix(platform: str) -> str:
    return ".exe" if platform.startswith("win") else ""


def binaries_from_archive(archive: Path, names: list[str], suffix: str) -> dict[str, bytes]:
    """
    Read each named binary out of a cargo-dist archive (``.tar.xz`` or
    ``.zip``), wherever in the archive it sits. A name the archive lacks is
    an error: a wheel missing a bundled binary is a broken channel, not a
    smaller one.
    """
    wanted = {name + suffix for name in names}
    found: dict[str, bytes] = {}
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as zf:
            for info in zf.infolist():
                base = info.filename.rsplit("/", 1)[-1]
                if base in wanted and not info.is_dir():
                    found[base] = zf.read(info)
    else:
        with tarfile.open(archive, "r:*") as tf:
            for member in tf.getmembers():
                base = member.name.rsplit("/", 1)[-1]
                if base in wanted and member.isfile():
                    found[base] = tf.extractfile(member).read()
    missing = sorted(wanted - found.keys())
    if missing:
        raise FileNotFoundError(f"{archive.name} lacks {', '.join(missing)}")
    return {name: found[name] for name in sorted(found)}


def build_wheel(
    *,
    version: str,
    binaries: dict[str, bytes],
    platform: str,
    out_dir: Path,
) -> Path:
    """
    Construct a platform wheel for *repoweave* containing the given binaries.

    Parameters
    ----------
    version:
        PEP 440 version string, e.g. ``"0.2.0"``.
    binaries:
        File name (``rwv``, ``rwv-each.exe``, ...) to binary contents.
    platform:
        Wheel platform tag, e.g. ``"manylinux_2_17_x86_64.manylinux2014_x86_64"``.
    out_dir:
        Directory where the ``.whl`` file will be written.

    Returns
    -------
    Path
        Path to the produced ``.whl`` file.
    """
    out_dir.mkdir(parents=True, exist_ok=True)

    dist_info = f"repoweave-{version}.dist-info"
    data_scripts = f"repoweave-{version}.data/scripts"

    # Build METADATA and WHEEL file contents.
    metadata_content = METADATA_TEMPLATE.format(version=version).encode()
    wheel_content = WHEEL_TEMPLATE.format(platform=platform).encode()

    # Paths inside the wheel archive.
    bin_entries = [(f"{data_scripts}/{name}", data) for name, data in binaries.items()]
    metadata_arc_path = f"{dist_info}/METADATA"
    wheel_arc_path = f"{dist_info}/WHEEL"
    record_arc_path = f"{dist_info}/RECORD"

    # Build RECORD lines for all files except RECORD itself.
    record_lines: list[str] = []
    for arc_path, content in [
        *bin_entries,
        (metadata_arc_path, metadata_content),
        (wheel_arc_path, wheel_content),
    ]:
        record_lines.append(f"{arc_path},{_sha256_record(content)},{len(content)}")
    # RECORD itself has no hash.
    record_lines.append(f"{record_arc_path},,")
    record_content = "\n".join(record_lines).encode() + b"\n"

    # Assemble the wheel (zip archive).
    wheel_filename = f"repoweave-{version}-py3-none-{platform}.whl"
    wheel_path = out_dir / wheel_filename

    with zipfile.ZipFile(wheel_path, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        # Binaries — need executable bits preserved via external_attr.
        for arc_path, data in bin_entries:
            bin_info = zipfile.ZipInfo(arc_path)
            # Unix mode 0o755 shifted into the high 16 bits of external_attr.
            bin_info.external_attr = (stat.S_IFREG | 0o755) << 16
            zf.writestr(bin_info, data)

        zf.writestr(metadata_arc_path, metadata_content)
        zf.writestr(wheel_arc_path, wheel_content)
        zf.writestr(record_arc_path, record_content)

    return wheel_path


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Build a platform-specific Python wheel bundling the rwv binaries.",
    )
    parser.add_argument("--version", required=True, help="Package version, e.g. 0.2.0")
    parser.add_argument(
        "--archive",
        required=True,
        type=Path,
        help="cargo-dist release archive (.tar.xz or .zip) for the platform",
    )
    parser.add_argument(
        "--platform",
        required=True,
        help=(
            "Wheel platform tag, e.g. manylinux_2_17_x86_64.manylinux2014_x86_64 "
            "or macosx_11_0_arm64 or win_amd64"
        ),
    )
    parser.add_argument(
        "--out",
        default="dist",
        type=Path,
        help="Output directory for the .whl file (default: dist/)",
    )

    args = parser.parse_args(argv)

    if not args.archive.is_file():
        print(f"error: archive not found: {args.archive}", file=sys.stderr)
        return 1
    try:
        binaries = binaries_from_archive(
            args.archive, bundled_binaries(), exe_suffix(args.platform)
        )
    except FileNotFoundError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    wheel = build_wheel(
        version=args.version,
        binaries=binaries,
        platform=args.platform,
        out_dir=args.out,
    )
    print(wheel)
    return 0


if __name__ == "__main__":
    sys.exit(main())
