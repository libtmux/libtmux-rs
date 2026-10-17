import re
from libtmux.common import TMUX_MIN_VERSION

_floor = re.fullmatch(r"([0-9]+)\.([0-9]+)([a-z]?)", str(TMUX_MIN_VERSION))
if _floor is None or (
    int(_floor[1]), int(_floor[2]), ord(_floor[3]) - 96 if _floor[3] else 0
) < (3, 3, 1):
    raise RuntimeError(
        "Python bridge requires libtmux with a hard tmux minimum of 3.3a or "
        "newer and per-command daemon version check; upgrade Python libtmux"
    )
