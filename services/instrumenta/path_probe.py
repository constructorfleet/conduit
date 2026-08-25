"""Boot-time PATH probe for runtimes.

Reports which runtimes (node, python3, uv) are available on the system
PATH so the UI can gate the "add stdio server" form to runtimes that
actually exist in the image.
"""

from __future__ import annotations

import shutil


_DEFAULT_RUNTIMES = ("python3", "node", "uv")


def probe_runtimes(runtimes: tuple[str, ...] = _DEFAULT_RUNTIMES) -> dict[str, bool]:
    """Return `{runtime: is_on_path}` for each requested runtime."""
    return {rt: shutil.which(rt) is not None for rt in runtimes}
