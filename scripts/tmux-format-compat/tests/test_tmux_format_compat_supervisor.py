"""Run each hostile-process case of the supervisor as a pytest case.

The cases kill, stop and fork their way out of the supervisor, so each runs in
a process of its own, under outer containment, and this file only reads its
status. Linux only.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(sys.platform != "linux", reason="the supervisor needs Linux")

CASES = Path(__file__).resolve().with_name("supervisor_cases.py")


def case_names() -> list[str]:
    if sys.platform != "linux":
        return []
    import supervisor_cases

    return list(supervisor_cases.TESTS)


@pytest.mark.parametrize("case", case_names())
def test_the_supervisor_case_passes(case: str) -> None:
    result = subprocess.run(
        [sys.executable, str(CASES), case],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, f"{case}:\n{result.stdout}\n{result.stderr}"
    assert f"ok {case}" in result.stdout
