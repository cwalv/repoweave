"""Run the native rwv binary via ``python -m repoweave``.

Uses ``os.execvp`` on Unix so the current process is replaced by the Rust
binary — signals flow straight through and there's no Python process left
waiting on the child. Windows falls back to ``subprocess.run`` because
``execvp`` is broken there. Same pattern as ``ruff.__main__`` and
``uv.__main__``.
"""

from __future__ import annotations

import os
import sys

from repoweave import _find_binary


if __name__ == "__main__":
    rwv = _find_binary()
    if sys.platform == "win32":
        import subprocess

        try:
            completed = subprocess.run([rwv, *sys.argv[1:]])
        except KeyboardInterrupt:
            sys.exit(130)
        sys.exit(completed.returncode)
    else:
        os.execvp(rwv, [rwv, *sys.argv[1:]])
