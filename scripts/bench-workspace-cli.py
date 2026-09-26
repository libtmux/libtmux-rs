#!/usr/bin/env python3
"""Measure an installed workspace CLI and verify its results on an owned socket."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--tmux", default=shutil.which("tmux"))
    parser.add_argument(
        "--tmuxp",
        type=Path,
        help="Pinned tmuxp 1.74.0 console executable for matched comparisons",
    )
    parser.add_argument("--samples", type=int, default=20)
    parser.add_argument("--load-samples", type=int, default=5)
    args = parser.parse_args()
    if min(args.samples, args.load_samples) < 1 or not args.tmux:
        parser.error("positive sample counts and an available tmux are required")
    binary = args.binary.resolve(strict=True)
    root = Path("/tmp/libtmux-rs-dev")
    root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="workspace-bench-", dir=root) as temporary:
        work = Path(temporary)
        socket = work / "tmux.sock"
        configs = work / "home" / ".tmuxp"
        configs.mkdir(parents=True)
        env = {
            key: value
            for key, value in os.environ.items()
            if key not in {"TMUX", "TMUX_PANE"}
        }
        env["PATH"] = (
            str(Path(args.tmux).resolve().parent) + os.pathsep + env.get("PATH", "")
        )
        env.update(
            HOME=str(configs.parent),
            TMUXP_CONFIGDIR=str(configs),
            XDG_CONFIG_HOME=str(work / "xdg"),
            LIBTMUX_TEST_TMUX=args.tmux,
            EDITOR="/bin/true",
            NO_COLOR="1",
        )

        def run(
            arguments: list[str], *, tmux: bool = False
        ) -> subprocess.CompletedProcess[str]:
            command = (
                [args.tmux, "-S", str(socket), "-f", "/dev/null"]
                if tmux
                else [str(binary)]
            )
            return subprocess.run(
                command + arguments,
                cwd=work,
                env=env,
                text=True,
                capture_output=True,
                check=True,
                timeout=30,
            )

        def value(arguments: list[str]) -> object:
            result = run(arguments)
            assert "\x1b" not in result.stdout
            return json.loads(result.stdout)

        timings: dict[str, list[float]] = {}

        def measured(name: str, arguments: list[str], samples: int) -> object:
            result: object = None
            for _ in range(samples):
                start = time.perf_counter_ns()
                result = run(arguments)
                timings.setdefault(name, []).append(
                    (time.perf_counter_ns() - start) / 1_000_000
                )
            return result

        for index in range(100):
            (configs / f"project-{index:03}.json").write_text(
                json.dumps(
                    {
                        "session_name": f"project-{index}",
                        "windows": [
                            {
                                "window_name": "editor",
                                "panes": [
                                    "echo needle" if index % 5 == 0 else "echo other"
                                ],
                            }
                        ],
                        "extension": {"text": "雪\t", "items": [True, None, 42]},
                    }
                )
            )
        source = str(configs / "project-000.json")
        measured("startup_version", ["--version"], args.samples)
        measured("list_100", ["ls", "--json", "--full"], args.samples)
        assert len(value(["ls", "--json"])["workspaces"]) == 100
        measured("search_100", ["search", "--json", "-F", "pane:needle"], args.samples)
        assert len(value(["search", "--json", "-F", "pane:needle"])) == 20
        measured("convert", ["convert", "--json", source], args.samples)
        assert value(["convert", "--json", source])["extension"]["items"] == [
            True,
            None,
            42,
        ]
        value(["debug-info", "--json"])
        value(["edit", "--json", source])
        for kind, config in [
            (
                "teamocil",
                {
                    "session": {
                        "name": "imported",
                        "windows": [
                            {"name": "editor", "splits": [{"cmd": "echo yes"}]}
                        ],
                    }
                },
            ),
            ("tmuxinator", {"name": "imported", "windows": [{"editor": "echo yes"}]}),
        ]:
            path = work / f"{kind}.json"
            path.write_text(json.dumps(config))
            assert (
                value(["import", kind, "--json", str(path)])["session_name"]
                == "imported"
            )

        run(["new-session", "-d", "-s", "benchmark-keeper", "/bin/sh"], tmux=True)
        try:
            for index in range(args.load_samples):
                name = f"measured-{index}"
                marker = work / f"marker-{index}"
                document = {
                    "session_name": name,
                    "windows": [
                        {
                            "window_name": "zero",
                            "window_index": 0,
                            "panes": [
                                f"printf ready > '{marker}'; exec sleep 60",
                                "blank",
                            ],
                        },
                        {"window_name": "three", "window_index": 3, "panes": ["blank"]},
                    ],
                }
                path = work / "load.json"
                path.write_text(json.dumps(document))
                start = time.perf_counter_ns()
                measured(
                    "load_2_windows_3_panes",
                    ["load", "-S", str(socket), "-d", "--json", str(path)],
                    1,
                )
                captured = measured(
                    "freeze_2_windows_3_panes",
                    ["freeze", "-S", str(socket), "--json", name],
                    1,
                )
                timings.setdefault("load_and_freeze", []).append(
                    (time.perf_counter_ns() - start) / 1_000_000
                )
                frozen = json.loads(captured.stdout)
                assert [window["window_index"] for window in frozen["windows"]] == [
                    0,
                    3,
                ]
                assert [len(window["panes"]) for window in frozen["windows"]] == [2, 1]
                deadline = time.monotonic() + 2
                while not marker.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                assert marker.read_text() == "ready"
                if index == 0:
                    frozen["session_name"] = "reloaded"
                    replay = work / "replay.json"
                    replay.write_text(json.dumps(frozen))
                    value(["load", "-S", str(socket), "-d", "--json", str(replay)])
                    reloaded = value(
                        ["freeze", "-S", str(socket), "--json", "reloaded"]
                    )
                    assert [len(window["panes"]) for window in reloaded["windows"]] == [
                        2,
                        1,
                    ]
                    run(["kill-session", "-t", "reloaded"], tmux=True)
                run(["kill-session", "-t", name], tmux=True)
            if env.get("TMUX_WORKSPACE_PYTHON"):
                shell = value(
                    [
                        "shell",
                        "-S",
                        str(socket),
                        "--json",
                        "--code",
                        "-c",
                        "print('bridge-verified')",
                        "benchmark-keeper",
                    ]
                )
                assert shell["stdout"].endswith("bridge-verified\n")
        finally:
            run(["kill-server"], tmux=True)

        comparator = None
        if args.tmuxp:
            reference = args.tmuxp.resolve(strict=True)
            version = subprocess.run(
                [str(reference), "--version"],
                env=env,
                text=True,
                capture_output=True,
                check=True,
            ).stdout.strip()
            assert "1.74.0" in version, version
            comparator = {
                "version": version,
                "binary_sha256": hashlib.sha256(reference.read_bytes()).hexdigest(),
            }
            seed = work / "tmux.conf"
            seed.write_text("set -g default-shell /bin/sh\n")
            fixture = work / "matched.json"
            fixture.write_text(
                json.dumps(
                    {
                        "session_name": "matched",
                        "start_directory": str(work),
                        "before_script": "sh -c 'printf ready > marker'",
                        "windows": [
                            {
                                "window_name": "zero",
                                "window_index": 0,
                                "panes": ["blank", "blank"],
                            },
                            {
                                "window_name": "three",
                                "window_index": 3,
                                "panes": ["blank"],
                            },
                        ],
                    }
                )
            )
            comparator["fixture_sha256"] = hashlib.sha256(
                fixture.read_bytes()
            ).hexdigest()
            for label, executable in [("rust", binary), ("tmuxp", reference)]:

                def compare_run(
                    arguments: list[str],
                    executable: Path = executable,
                ) -> subprocess.CompletedProcess[str]:
                    return subprocess.run(
                        [str(executable)] + arguments,
                        cwd=work,
                        env=env,
                        text=True,
                        capture_output=True,
                        check=True,
                        timeout=30,
                    )

                for name, command in [
                    ("startup_version", ["--version"]),
                    ("list_100", ["ls", "--json", "--full"]),
                    ("search_100", ["search", "--json", "-F", "pane:needle"]),
                ]:
                    for _ in range(args.samples):
                        start = time.perf_counter_ns()
                        result = compare_run(command)
                        timings.setdefault(f"matched_{label}_{name}", []).append(
                            (time.perf_counter_ns() - start) / 1_000_000
                        )
                        if name == "list_100":
                            assert len(json.loads(result.stdout)["workspaces"]) == 100
                        elif name == "search_100":
                            assert len(json.loads(result.stdout)) == 20
                for index in range(args.load_samples):
                    cold_socket = work / f"{label}-{index}.sock"
                    capture = work / f"{label}-{index}.json"
                    (work / "marker").unlink(missing_ok=True)
                    load = [
                        "load",
                        "-S",
                        str(cold_socket),
                        "-f",
                        str(seed),
                        "-d",
                        "-y",
                        "--no-progress",
                        str(fixture),
                    ]
                    freeze = [
                        "freeze",
                        "-S",
                        str(cold_socket),
                        "-f",
                        "json",
                        "-o",
                        str(capture),
                        "-y",
                        "--force",
                        "-q",
                        "matched",
                    ]
                    assert not cold_socket.exists()
                    try:
                        elapsed = 0.0
                        for name, command in [
                            ("cold_load", load),
                            ("file_freeze", freeze),
                        ]:
                            start = time.perf_counter_ns()
                            compare_run(command)
                            duration = (time.perf_counter_ns() - start) / 1_000_000
                            timings.setdefault(f"matched_{label}_{name}", []).append(
                                duration
                            )
                            elapsed += duration
                            if name == "cold_load":
                                topology = subprocess.run(
                                    [
                                        args.tmux,
                                        "-S",
                                        str(cold_socket),
                                        "list-panes",
                                        "-a",
                                        "-F",
                                        "#{session_name}:#{window_index}",
                                    ],
                                    env=env,
                                    text=True,
                                    capture_output=True,
                                    check=True,
                                ).stdout.splitlines()
                                assert sorted(topology) == [
                                    "matched:0",
                                    "matched:0",
                                    "matched:3",
                                ], topology
                                assert (work / "marker").read_text() == "ready"
                        timings.setdefault(
                            f"matched_{label}_load_and_freeze", []
                        ).append(elapsed)
                        frozen = json.loads(capture.read_text())
                        assert frozen["session_name"] == "matched"
                        assert [
                            len(window["panes"]) for window in frozen["windows"]
                        ] == [2, 1]
                    finally:
                        subprocess.run(
                            [args.tmux, "-S", str(cold_socket), "kill-server"],
                            env=env,
                            capture_output=True,
                            check=False,
                        )
            comparator["boundaries"] = (
                "Identical fixture and flags; cold socket for each load, JSON file freeze, correctness and cleanup outside timers; combined is the sum of command durations."
            )

        summary = {
            name: {
                "samples": len(values),
                "samples_ms": values,
                "stdev_ms": statistics.stdev(values) if len(values) > 1 else 0.0,
                "median_ms": statistics.median(values),
                "mean_ms": statistics.mean(values),
                "min_ms": min(values),
                "max_ms": max(values),
                "p95_ms": sorted(values)[math.ceil(0.95 * len(values)) - 1],
            }
            for name, values in timings.items()
        }
        print(
            json.dumps(
                {
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "version": run(["--version"]).stdout.strip(),
                    "tmux_version": run(["-V"], tmux=True).stdout.strip(),
                    "correctness": "passed",
                    "comparator": comparator,
                    "timings": summary,
                },
                indent=2,
            )
        )


if __name__ == "__main__":
    main()
