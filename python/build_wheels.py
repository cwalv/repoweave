#!/usr/bin/env python3
"""
Build platform-specific Python wheels that bundle the rwv binary.

Each wheel follows the "data/scripts" layout used by ruff and similar
native-binary Python distributions:

    repoweave-{version}.data/scripts/rwv   <- the binary (lands in PATH)
    repoweave-{version}.dist-info/METADATA
    repoweave-{version}.dist-info/WHEEL
    repoweave-{version}.dist-info/RECORD

No Python source is included — pip/uv simply install the binary.

Usage
-----
    python build_wheels.py --version 0.2.0 --binary /tmp/rwv --platform manylinux_2_17_x86_64.manylinux2014_x86_64 --out dist/

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
import zipfile
from pathlib import Path


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


def build_wheel(
    *,
    version: str,
    binary_path: Path,
    platform: str,
    out_dir: Path,
) -> Path:
    """
    Construct a platform wheel for *repoweave* containing only the rwv binary.

    Parameters
    ----------
    version:
        PEP 440 version string, e.g. ``"0.2.0"``.
    binary_path:
        Absolute path to the compiled rwv (or rwv.exe) binary.
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

    # Binary name inside the wheel mirrors the on-disk name (rwv / rwv.exe).
    bin_name = binary_path.name

    # Read the binary once; we need its bytes for hashing and for the zip entry.
    bin_bytes = binary_path.read_bytes()

    # Build METADATA and WHEEL file contents.
    metadata_content = METADATA_TEMPLATE.format(version=version).encode()
    wheel_content = WHEEL_TEMPLATE.format(platform=platform).encode()

    # Paths inside the wheel archive.
    bin_arc_path = f"{data_scripts}/{bin_name}"
    metadata_arc_path = f"{dist_info}/METADATA"
    wheel_arc_path = f"{dist_info}/WHEEL"
    record_arc_path = f"{dist_info}/RECORD"

    # Build RECORD lines for all files except RECORD itself.
    record_lines: list[str] = []
    for arc_path, content in [
        (bin_arc_path, bin_bytes),
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
        # Binary — needs executable bits preserved via external_attr.
        bin_info = zipfile.ZipInfo(bin_arc_path)
        # Unix mode 0o755 shifted into the high 16 bits of external_attr.
        bin_mode = stat.S_IFREG | 0o755
        bin_info.external_attr = bin_mode << 16
        zf.writestr(bin_info, bin_bytes)

        zf.writestr(metadata_arc_path, metadata_content)
        zf.writestr(wheel_arc_path, wheel_content)
        zf.writestr(record_arc_path, record_content)

    return wheel_path


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Build a platform-specific Python wheel bundling the rwv binary.",
    )
    parser.add_argument("--version", required=True, help="Package version, e.g. 0.2.0")
    parser.add_argument(
        "--binary",
        required=True,
        type=Path,
        help="Path to the compiled rwv (or rwv.exe) binary",
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

    if not args.binary.is_file():
        print(f"error: binary not found: {args.binary}", file=sys.stderr)
        return 1

    wheel = build_wheel(
        version=args.version,
        binary_path=args.binary,
        platform=args.platform,
        out_dir=args.out,
    )
    print(wheel)
    return 0


if __name__ == "__main__":
    sys.exit(main())
