"""
repoweave - A cross-repo workspace manager.

This is a thin Python wrapper that downloads and runs the pre-built
repoweave (rwv) binary from GitHub Releases. It enables `uvx repoweave`
and `pipx run repoweave` without requiring a Rust toolchain.
"""

from __future__ import annotations

import os
import platform
import sys
import sysconfig


def _find_binary() -> str:
    """Locate the rwv binary bundled in the package's scripts/data directory."""
    # When installed via pip/pipx/uvx, the binary ends up in the scripts dir
    scripts_dir = sysconfig.get_path("scripts")
    if scripts_dir:
        bin_name = "rwv.exe" if sys.platform == "win32" else "rwv"
        candidate = os.path.join(scripts_dir, bin_name)
        if os.path.isfile(candidate):
            return candidate

    # Fallback: check next to this file (for development / editable installs)
    pkg_dir = os.path.dirname(os.path.abspath(__file__))
    bin_name = "rwv.exe" if sys.platform == "win32" else "rwv"
    candidate = os.path.join(pkg_dir, "bin", bin_name)
    if os.path.isfile(candidate):
        return candidate

    raise FileNotFoundError(
        f"Could not find the rwv binary. "
        f"Platform: {platform.system()} {platform.machine()}. "
        f"Please report this issue at https://github.com/cwalv/repoweave/issues"
    )


def main() -> None:
    """Entry point for the `rwv` console script."""
    import subprocess

    binary = _find_binary()
    result = subprocess.run([binary, *sys.argv[1:]])
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
