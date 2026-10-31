#!/usr/bin/env python3
"""Tests for the environment `tmuxp-parity.py` and `bench-workspace-cli.py` give
the tmux and workspace processes they start.

Both run an installed binary outside Cargo, so nothing sandboxes them but what
they build: a child that inherited `TMUX` or `LIBTMUX_SOCKET` would follow it
to the reader's own server.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import stat
import sys
import tempfile
import unittest
from unittest import mock

SELECTORS = ("TMUX", "TMUX_PANE", "LIBTMUX_SOCKET", "LIBTMUX_SOCKET_PATH")


def load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(
        name, pathlib.Path(__file__).with_name(filename)
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


parity = load("tmuxp_parity", "tmuxp-parity.py")
bench = load("bench_workspace_cli", "bench-workspace-cli.py")


class ScrubbedEnvironmentTests(unittest.TestCase):
    def test_every_server_selector_is_dropped_and_nothing_else(self) -> None:
        for module in (parity, bench):
            for name in SELECTORS:
                with self.subTest(script=module.__name__, variable=name):
                    kept = module.scrubbed_environment({name: "value", "KEEP": "1"})
                    self.assertEqual(kept, {"KEEP": "1"})

    def test_the_parity_environment_carries_none_of_them(self) -> None:
        inherited = {name: "value" for name in SELECTORS}
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(
            os.environ, inherited
        ):
            env = parity.build_env(pathlib.Path(directory))
        for name in SELECTORS:
            with self.subTest(variable=name):
                self.assertNotIn(name, env)
        self.assertIn("TMUX_TMPDIR", env)


class BenchEnvironmentTests(unittest.TestCase):
    def test_the_benchmark_environment_carries_none_of_them(self) -> None:
        inherited = {name: "value" for name in SELECTORS}
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(
            os.environ, inherited
        ):
            work = pathlib.Path(directory)
            env = bench.build_env(work, work / "home" / ".tmuxp", "tmux")
            tmpdir = pathlib.Path(env["TMUX_TMPDIR"])
            mode = stat.S_IMODE(tmpdir.stat().st_mode)
            parent = tmpdir.parent
        for name in SELECTORS:
            with self.subTest(variable=name):
                self.assertNotIn(name, env)
        self.assertEqual(parent, work)
        self.assertEqual(mode, 0o700)


class PrivateTmuxTmpdirTests(unittest.TestCase):
    def test_the_directory_is_owner_only_whatever_the_umask(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            work = pathlib.Path(directory)
            previous = os.umask(0o277)
            try:
                tmpdir = bench.private_tmux_tmpdir(work)
            finally:
                os.umask(previous)
            self.assertEqual(tmpdir.parent, work)
            self.assertEqual(stat.S_IMODE(tmpdir.stat().st_mode), 0o700)


if __name__ == "__main__":
    unittest.main()
