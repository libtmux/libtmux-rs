#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Run every shipped example to completion and prove it left nothing behind.

    run_examples.py

`cargo test --all-targets` compiles an example and never runs it, so an example
that fails at runtime passes every other gate. This runs each one against a
server this script owns rather than whatever the reader happens to have, and
fails when one exits nonzero or leaves a socket in the shared root.

`TABLE` is the other half of that. Naming the examples to run made adding one a
silent no-op: the new file compiled, this gate never called it, and nothing
said so. Three `tmux-mcp` examples sat unrun that way. So the table is checked
against `cargo metadata` before anything runs, in both directions: an example
with no row fails, and a row naming no example fails, which is what catches a
rename rather than only an addition.
"""

from __future__ import annotations

import contextlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fixture-root"))
from fixture_root import abandoned

ROOT = Path(__file__).resolve().parents[2]
DEV_ROOT = Path("/tmp/libtmux-rs-dev")

# crate | example | features | arguments | stdin driver | must print | needs
#
# Blank lines and `#` comments are ignored; every other row must name an
# example that exists, and every example must have a row.
#
# The last column is a list of words: `control` for an example that needs
# control acquisition, `pane` for one that reads `$TMUX` as a process inside a
# pane does and is handed this script's server by name.
#
# The column before it is what the example must print, and it answers two
# questions a leak check cannot.
#
# Containment: `inspect` and `find` resolve their server with
# `Server::from_env().or_else(|_| Server::new())`, so a `$TMUX` this script
# failed to hand them would send them to the default server of whoever runs
# this, where they would run perfectly, create nothing, and leave no socket
# behind to notice. Naming a session this script made can only come from this
# server.
#
# Demonstration: an example that runs and shows nothing passes an exit-code
# check. `find` searched for `sh` against a fixture running the login shell and
# matched nothing every time; `scratch` printed a line count and none of the
# session it built. Naming what each is for makes that a failure.
TABLE = """
libtmux-tokio | api_construction | query | | | construction: private daemon started by new_session
libtmux-tokio | api_listings | query | | | listings: 1 session, 2 windows, 2 panes
libtmux-tokio | api_creation | query | | | creation: 2 windows, 4 panes
libtmux-tokio | api_query | query | | | query: prod-api
libtmux-tokio | api_cardinality | query | | | cardinality: none, one, multiple
libtmux-tokio | api_input | query | | | input: C-c is text
libtmux-tokio | api_capture | query | | | capture: first line, second line
libtmux-tokio | inspect  |                                               |    |               | examples | pane
libtmux-tokio | find     | query                                         | sh |               | in window | pane
libtmux-tokio | scratch  | test-support                                  |    |               | hello from tmux
libtmux-tokio | sweep    | test-support                                  |    |               | fixture
libtmux-tokio | watch    | control-mode,test-support                     |    |               | on the same socket | control
libtmux-tokio | matrix   | plan,control-mode,test-support,query |    |               | every mode built the same thing: true
libtmux-tokio | orchestrate | test-support                              |    |               | 3 of 3 jobs finished
libtmux-tokio | recover  | test-support                                  |    |               | is_object_gone: false
libtmux | api_mixed | blocking,tokio,test-support | | | mixed runtime
tmux-mcp | budget   |                                               |    |               | of it output schemas
tmux-mcp | surface  |                                               |    |               | answers with:
tmux-mcp | readonly |                                               |    | mcp_handshake | serverInfo
tmux-mcp | run_and_wait |                                           |    |               | waited: Matched | control
tmux-mcp | follow   |                                               |    |               | saw 200 of 200 lines, missed: false | control
tmux-mcp | refused  |                                               |    |               | send_keys refused: Mcp error: -32602: tool send_keys is not offered
tmux-workspace | build |                                              |    |               | built editor: 2 panes
tmux-workspace | strict |                                             |    |               | strict: refused: workspace configuration is invalid at line 8
"""

# `readonly` is a server: it runs until its client hangs up, and hanging up
# before it has initialised is the one case it reports as an error, because a
# client that never spoke never started. So the gate has to be that client.
# Waiting for the reply rather than sleeping a fixed span keeps this from
# asserting how fast the machine is; the cap only stops a wedge from hanging
# the gate. The request is written before the polling starts, and that ordering
# is what makes the cap safe rather than merely generous: the line is already
# in the pipe, so a poll that gives up still leaves a server that reads it,
# answers, sees EOF and exits clean.
HANDSHAKE = (
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":'
    '{"protocolVersion":"2024-11-05","capabilities":{},'
    '"clientInfo":{"name":"run-examples","version":"0"}}}\n'
)
HANDSHAKE_POLLS = 600
HANDSHAKE_PAUSE = 0.05
DRIVERS = ("mcp_handshake",)

SANDBOX_FIXTURE_PROBE = "examples (2 windows"


@dataclass(frozen=True)
class Row:
    crate: str
    example: str
    features: str
    arguments: str
    driver: str
    expect: str
    needs: str

    @property
    def key(self) -> str:
        return f"{self.crate}|{self.example}"

    def needs_word(self, word: str) -> bool:
        return word in self.needs.split()


def parse_table(text: str) -> list[Row]:
    """The rows of `text`: `#` comments and blank lines dropped, fields trimmed.

    >>> parse_table("# note\\nlibtmux | scratch | test-support | | | hello # why\\n")
    [Row(crate='libtmux', example='scratch', features='test-support', arguments='', driver='', expect='hello', needs='')]
    """
    rows = []
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = [field.strip() for field in line.split("|")]
        fields += [""] * (7 - len(fields))
        rows.append(Row(*fields[:7]))
    return rows


def compare(declared: Iterable[str], existing: Iterable[str]) -> tuple[list[str], list[str]]:
    """The examples with no row, and the rows with no example.

    >>> compare(["a|x", "a|y"], ["a|y", "a|z"])
    (['a|z'], ['a|x'])
    """
    declared, existing = set(declared), set(existing)
    return sorted(existing - declared), sorted(declared - existing)


def workspace_examples() -> list[str]:
    """What the workspace ships, asked of Cargo rather than of the filesystem.

    A crate may place its examples anywhere, and a second crate growing one is
    exactly the case a path glob under `crates/libtmux` missed.
    """
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return [
        f"{package['name']}|{target['name']}"
        for package in json.loads(output)["packages"]
        for target in package["targets"]
        if "example" in target["kind"]
    ]


VERSION = re.compile(r"(next-)?([0-9]+)\.([0-9]+)([a-z]|-rc[0-9]*)?")


def control_qualified(version: str) -> bool:
    """Whether a daemon ranks at or above 3.7 final, as `since::CONTROL_ACQUISITION` says.

    That is `3.7` and later numbered releases, plus a development build of a
    later minor, such as `next-3.9`. `next-3.7` and 3.7 release candidates rank
    below 3.7 final.

    >>> [control_qualified(v) for v in ("3.8", "3.7", "3.7b", "3.7-rc1", "next-3.7", "next-3.9", "3.6b")]
    [True, True, True, False, False, True, False]
    """
    match = VERSION.fullmatch(version.strip())
    if match is None:
        return False
    development, major, minor, suffix = match.groups()
    release = (int(major), int(minor))
    return release > (3, 7) or (
        release == (3, 7) and not development and not (suffix or "").startswith("-rc")
    )


def cargo_options(row: Row) -> list[str]:
    options = ["--quiet", "--manifest-path", f"crates/{row.crate}/Cargo.toml"]
    options += ["--example", row.example]
    if row.features:
        options += ["--features", row.features]
    return options


class Fixture:
    """A tmux server this script owns, which the `pane` examples are handed by name."""

    def __init__(self, directory: Path) -> None:
        self.directory = directory
        self.socket = directory / "server.sock"
        self.tmux_value = ""
        self.version = ""

    def tmux(self, *arguments: str) -> str:
        return subprocess.run(
            ["tmux", "-f", "/dev/null", "-S", str(self.socket), *arguments],
            capture_output=True,
            stdin=subprocess.DEVNULL,
            text=True,
            check=True,
        ).stdout.strip()

    def start(self) -> None:
        # Who to ask about, later. A run still going owns its directory; one
        # whose owner is gone left it behind. `just fixture-root` decides the
        # same way.
        (self.directory / "owner").write_text(f"{os.getpid()}\n")
        # `inspect` and `find` read `$TMUX`, which is how a process inside a
        # pane finds the server it belongs to. Handing them a socket this
        # script owns keeps them off the reader's own server while still
        # exercising the path they document. The second window runs `sh`
        # rather than the login shell because `find` searches for it.
        self.tmux("new-session", "-d", "-s", "examples")
        self.tmux("new-window", "-d", "-n", "second", "sh")
        pid = self.tmux("display-message", "-p", "#{pid}")
        # What a pane of that server exports. Cargo's sandbox removes `TMUX`, so
        # the `pane` examples are handed it by name through `LIBTMUX_SANDBOX_TMUX`.
        self.tmux_value = f"{self.socket},{pid},0"
        self.version = self.tmux("display-message", "-p", "#{version}")

    def stop(self) -> None:
        subprocess.run(
            ["tmux", "-f", "/dev/null", "-S", str(self.socket), "kill-server"],
            capture_output=True,
            stdin=subprocess.DEVNULL,
            check=False,
        )


def cargo_run(row: Row) -> list[str]:
    """The `cargo run` command for a row; its environment is `run_environment`."""
    return ["cargo", "run", *cargo_options(row), "--", *row.arguments.split()]


def run_environment(row: Row, fixture: Fixture) -> dict[str, str]:
    """`cargo run`, naming the fixture server for a `pane` example and no other."""
    environ = dict(os.environ)
    environ.pop("LIBTMUX_SANDBOX_TMUX", None)
    if row.needs_word("pane"):
        environ["LIBTMUX_SANDBOX_TMUX"] = fixture.tmux_value
    return environ


def run_plain(row: Row, fixture: Fixture, out: Path) -> bool:
    with (
        out.open("wb") as log,
        subprocess.Popen(
            cargo_run(row),
            cwd=ROOT,
            env=run_environment(row, fixture),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            bufsize=0,
        ) as child,
    ):
        assert child.stdout is not None
        # Standard output is what the example must print; standard error goes
        # straight to the terminal.
        while chunk := child.stdout.read(65536):
            # Text this script printed earlier comes out before the example's.
            sys.stdout.flush()
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
            log.write(chunk)
    return child.returncode == 0


def run_driven(row: Row, fixture: Fixture, out: Path) -> bool:
    """Run an example whose stdin is written by a driver, and only stdout is kept."""
    if row.driver not in DRIVERS:
        raise SystemExit(f"unknown driver for {row.key}: {row.driver}")
    out.write_bytes(b"")
    with (
        out.open("wb") as log,
        subprocess.Popen(
            cargo_run(row),
            cwd=ROOT,
            env=run_environment(row, fixture),
            stdin=subprocess.PIPE,
            stdout=log,
        ) as child,
    ):
        assert child.stdin is not None
        # The request is in the pipe before the polling starts.
        with contextlib.suppress(BrokenPipeError):
            child.stdin.write(HANDSHAKE.encode())
            child.stdin.flush()
        for _ in range(HANDSHAKE_POLLS):
            if b'"result"' in out.read_bytes():
                break
            time.sleep(HANDSHAKE_PAUSE)
        with contextlib.suppress(BrokenPipeError):
            child.stdin.close()
    return child.returncode == 0


def sandbox_probe(fixture: Fixture, probe: Path) -> str | None:
    """Run `inspect` with every variable that selects a server aimed at the fixture.

    None of them hands the fixture over by name, so the example must find no
    server on the sandbox's default socket. The `pane` row of the same example
    is the control. Returns the name of the failure, or `None`.
    """
    environ = dict(os.environ)
    environ.update(
        TMUX=fixture.tmux_value,
        TMUX_PANE="%0",
        LIBTMUX_SOCKET_PATH=str(fixture.socket),
        LIBTMUX_SOCKET="not-the-default",
    )
    environ.pop("LIBTMUX_SANDBOX_TMUX", None)
    with probe.open("wb") as log:
        subprocess.run(
            [
                "cargo",
                "run",
                "--quiet",
                "--manifest-path",
                "crates/libtmux-tokio/Cargo.toml",
                "--example",
                "inspect",
            ],
            cwd=ROOT,
            env=environ,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            check=False,
        )
    text = probe.read_text(errors="replace")
    if SANDBOX_FIXTURE_PROBE in text:
        print("a variable led an example to the fixture server", file=sys.stderr)
        return "sandbox/server"
    if not re.search(r"^no tmux server at .*/default$", text, re.MULTILINE):
        print("the example did not look on the sandbox default socket", file=sys.stderr)
        return "sandbox/default"
    print("no server found on the sandbox default socket")
    return None


def main() -> int:
    os.chdir(ROOT)
    rows = parse_table(TABLE)
    unlisted, missing = compare((row.key for row in rows), workspace_examples())
    if unlisted or missing:
        print("the example table does not match the workspace:", file=sys.stderr)
        if unlisted:
            print(f"  shipped but never run: {' '.join(unlisted)}", file=sys.stderr)
        if missing:
            print(f"  named here but absent:  {' '.join(missing)}", file=sys.stderr)
        print(
            "\nAdd a row to `TABLE` in this script, or remove the stale one.\n"
            "A row names the crate, the example, its features, its arguments,\n"
            "and optionally a function to write its stdin.",
            file=sys.stderr,
        )
        return 1

    DEV_ROOT.mkdir(parents=True, exist_ok=True)
    run_dir = Path(tempfile.mkdtemp(prefix="examples.", dir=DEV_ROOT))
    fixture = Fixture(run_dir)
    failures: list[str] = []
    try:
        fixture.start()
        qualified = control_qualified(fixture.version)

        # Built before any of them runs, so the run phase is the example's own
        # cost. A driven example measures how long its server takes to answer,
        # and a cold build inside that window reads as a server that never did.
        print(f"building {len(rows)} examples")
        for row in rows:
            built = subprocess.run(["cargo", "build", *cargo_options(row)], cwd=ROOT, check=False)
            if built.returncode != 0:
                return built.returncode

        print("\n=== sandbox: variables that name a server are not followed")
        failure = sandbox_probe(fixture, run_dir / "probe.out")
        if failure:
            failures.append(failure)

        for row in rows:
            print(f"\n=== example {row.crate}/{row.example}")
            if row.needs_word("control") and not qualified:
                print("positive control example requires a daemon ranking at or above tmux 3.7")
                continue
            out = run_dir / f"{row.example}.out"
            if row.driver:
                ran = run_driven(row, fixture, out)
                if ran:
                    print(f"answered {out.stat().st_size} bytes and shut down cleanly")
            else:
                ran = run_plain(row, fixture, out)
            if not ran:
                failures.append(row.key.replace("|", "/"))
                if row.driver:
                    print(
                        "\n".join(out.read_text(errors="replace").splitlines()[:5]), file=sys.stderr
                    )
            elif row.expect and row.expect not in out.read_text(errors="replace"):
                print(f"did not print {row.expect}", file=sys.stderr)
                failures.append(row.key.replace("|", "/"))
    finally:
        fixture.stop()
        shutil.rmtree(run_dir, ignore_errors=True)

    leaked = [str(entry) for entry in abandoned(DEV_ROOT)]
    status = 0
    if failures:
        print(f"\nexamples exited nonzero: {' '.join(failures)}", file=sys.stderr)
        status = 1
    if leaked:
        print(f"\n{DEV_ROOT} holds what nobody is using:\n" + "\n".join(leaked), file=sys.stderr)
        print(
            "\ntmux does not unlink its socket when the server exits, so whatever\n"
            "named one owns removing it. A daemon may still be answering on one of\n"
            "these, holding a pseudo-terminal per pane; check with `tmux -S <path>\n"
            "kill-server` before removing the directory.",
            file=sys.stderr,
        )
        status = 1
    if status == 0:
        print("\nevery eligible example ran and left nothing behind")
    return status


if __name__ == "__main__":
    raise SystemExit(main())
