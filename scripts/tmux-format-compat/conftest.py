"""The supervisor and its cases use Linux system calls, so another platform skips them."""

import sys

collect_ignore_glob: list[str] = []
if sys.platform != "linux":
    collect_ignore_glob = [
        "tmux_format_compat_supervisor.py",
        "tests/supervisor_cases.py",
        "tests/test_tmux_format_compat_supervisor.py",
    ]
