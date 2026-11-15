"""A benchmark that slows past its neighbours fails the run; one that moves with them does not."""

from __future__ import annotations

import json
from pathlib import Path

import bench_regressions
import pytest


def record(root: Path, name: str, change: float, mean: float = 1e6) -> None:
    for label, value in (("change", change), ("new", mean)):
        path = root / name / label
        path.mkdir(parents=True)
        (path / "estimates.json").write_text(
            json.dumps({"mean": {"point_estimate": value}}), encoding="utf-8"
        )


def test_the_first_run_has_nothing_to_compare_with(tmp_path: Path) -> None:
    assert bench_regressions.main([str(tmp_path)]) == 0


def test_a_machine_wide_slowdown_inside_the_limit_passes(tmp_path: Path) -> None:
    for name in ("a", "b", "c"):
        record(tmp_path, name, 0.8)
    assert bench_regressions.main([str(tmp_path)]) == 0


def test_one_benchmark_past_the_limit_fails(tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
    for name in ("a", "b", "c"):
        record(tmp_path, name, 0.0)
    record(tmp_path, "slow", 2.5)
    assert bench_regressions.main([str(tmp_path)]) == 1
    assert "slow is 3.50x slower" in capsys.readouterr().err


def test_every_benchmark_moving_together_past_the_machine_limit_fails(tmp_path: Path) -> None:
    for name in ("a", "b", "c"):
        record(tmp_path, name, 3.5)
    assert bench_regressions.main([str(tmp_path)]) == 1
