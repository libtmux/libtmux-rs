#!/usr/bin/env python3
"""Run the unchanged ordinary example with owned endpoint defaults."""

from contextlib import contextmanager
import json
import os
import re
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
VARIABLES = ("LIBTMUX_SOCKET_PATH", "LIBTMUX_SOCKET_NAME", "TMUX", "TMUX_PANE", "TMUX_TMPDIR")


def clean_environment():
    environment = os.environ.copy()
    for key in VARIABLES:
        environment.pop(key, None)
    return environment


def run(arguments, *, environment, check=True):
    return subprocess.run(arguments, cwd=ROOT, env=environment, check=check,
                          text=True, capture_output=True, timeout=30)


@contextmanager
def fixture(tmux):
    root = Path("/tmp/libtmux-rs-dev")
    root.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="default-example-", dir=root))
    socket = directory / "server.sock"
    (directory / "owner").write_text(str(os.getpid()))
    config = directory / "tmux.conf"
    config.write_text("set -g default-shell /bin/sh\nset -g exit-empty off\n")
    daemon = None
    failure = None
    cleanup_errors = []
    environment = clean_environment()
    try:
        daemon = subprocess.Popen([tmux, "-D", "-S", str(socket), "-f", str(config)],
                                  env=environment, stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  start_new_session=True)
        deadline = time.monotonic() + 5
        while True:
            probe = run([tmux, "-S", str(socket), "show-options", "-g"],
                        environment=environment, check=False)
            if probe.returncode == 0:
                break
            if daemon.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError(f"fixture startup failed: {probe.stderr}")
            time.sleep(0.01)
        yield directory, socket
    except BaseException as error:
        failure = error
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                stopped = run([tmux, "-S", str(socket), "kill-server"],
                              environment=environment, check=False)
                if stopped.returncode:
                    cleanup_errors.append(RuntimeError(f"kill-server failed: {stopped.stderr}"))
                daemon.wait(timeout=5)
            except BaseException as error:
                cleanup_errors.append(error)
                if daemon.poll() is None:
                    try:
                        daemon.kill()
                        daemon.wait(timeout=5)
                    except BaseException as forced_error:
                        cleanup_errors.append(forced_error)
        if daemon is None or daemon.poll() is not None:
            try:
                shutil.rmtree(directory)
            except OSError as error:
                cleanup_errors.append(error)
        else:
            cleanup_errors.append(RuntimeError("fixture daemon still runs; directory retained"))
        if cleanup_errors:
            if failure is not None:
                cleanup_errors.insert(0, failure)
            raise BaseExceptionGroup("example or fixture cleanup failed", cleanup_errors)
        if failure is not None:
            raise failure
        assert not directory.exists(), "fixture directory remains"


def main():
    source = (ROOT / "crates/libtmux/examples/default_session.rs").read_text()
    program = source[source.index("use libtmux"):].strip()
    for readme in ("README.md", "crates/libtmux/README.md"):
        block = re.search(r"```rust,no_run\n(.*?)\n```", (ROOT / readme).read_text(), re.S)
        assert block is not None and block.group(1).strip() == program, readme
    before = os.environ.copy()
    tmux = shutil.which(os.environ.get("LIBTMUX_TEST_TMUX", "tmux"))
    if tmux is None:
        raise RuntimeError("tmux executable not found")
    subprocess.run(["cargo", "build", "--locked", "--offline", "-p", "libtmux",
                    "--example", "default_session"], cwd=ROOT, check=True)
    metadata = run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                   environment=clean_environment())
    binary = Path(json.loads(metadata.stdout)["target_directory"]) / "debug/examples/default_session"
    for fail_body in (False, True):
        with fixture(tmux) as (directory, socket):
            environment = clean_environment()
            environment["LIBTMUX_SOCKET_PATH"] = str(socket)
            environment["LIBTMUX_SOCKET_NAME"] = "ignored/invalid"
            environment["TMUX"] = "ignored-invalid-context"
            environment["TMUX_PANE"] = "%999"
            if fail_body:
                wrapper = directory / "tmux"
                wrapper.write_text("#!/bin/sh\nfor argument do\n"
                                   "  if [ \"$argument\" = list-windows ]; then\n"
                                   "    printf 'intentional example body failure\\n' >&2\n"
                                   "    exit 42\n  fi\ndone\n"
                                   f"exec {shlex.quote(tmux)} \"$@\"\n")
                wrapper.chmod(0o700)
                environment["PATH"] = f"{directory}:{environment.get('PATH', '')}"
            else:
                environment["PATH"] = f"{Path(tmux).parent}:{environment.get('PATH', '')}"
            result = run([str(binary)], environment=environment, check=False)
            assert "created scoped session $" in result.stdout, result
            if fail_body:
                assert result.returncode != 0, result
                assert "intentional example body failure" in result.stderr, result
            else:
                assert result.returncode == 0, result.stderr
                assert "windows: 1" in result.stdout, result.stdout
            remaining = run([tmux, "-S", str(socket), "list-sessions", "-F", "#{session_name}"],
                            environment=clean_environment())
            assert not remaining.stdout.strip(), remaining.stdout
        print(f"PASS unchanged example {'failure' if fail_body else 'success'}; session, daemon and files cleaned")
    assert os.environ == before, "harness changed the host environment"
    print("PASS host environment preserved")


if __name__ == "__main__":
    main()
