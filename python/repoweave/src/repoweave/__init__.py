"""
repoweave - A cross-repo workspace manager.

The installable command is the native `rwv` binary shipped in the wheel's
`.data/scripts/` directory (via cargo-dist / build_wheels.py). This Python
package exists so `python -m repoweave` works (see ``__main__.py``) and so
``import repoweave`` succeeds.

This module intentionally has no ``main()`` entry point: running through a
Python wrapper defeats signal forwarding (``subprocess.run`` doesn't
propagate SIGTERM/SIGINT to the child), which breaks shell hooks that
invoke rwv. The ruff and uv projects ship in the same way.
"""

from __future__ import annotations

import os
import platform
import sys
import sysconfig


def _find_binary() -> str:
    """Locate the rwv binary bundled in the package's scripts/data directory."""
    scripts_dir = sysconfig.get_path("scripts")
    if scripts_dir:
        bin_name = "rwv.exe" if sys.platform == "win32" else "rwv"
        candidate = os.path.join(scripts_dir, bin_name)
        if os.path.isfile(candidate):
            return candidate

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
