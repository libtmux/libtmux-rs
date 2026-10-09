#!/usr/bin/env python3
"""Execute ordinary lifecycle source and explicit whole-server adoption."""

import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("default_example", ROOT / "scripts/test-default-example.py")
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


def main():
    before = os.environ.copy()
    tmux = shutil.which(os.environ.get("LIBTMUX_TEST_TMUX", "tmux"))
    assert tmux is not None
    subprocess.run(["cargo", "build", "--locked", "--offline", "-p", "libtmux",
                    "--example", "lifecycle", "--example", "adopt_server", "--example", "adopt_resources"], cwd=ROOT, check=True)
    metadata = HARNESS.run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                           environment=HARNESS.clean_environment())
    binaries = Path(json.loads(metadata.stdout)["target_directory"]) / "debug/examples"
    for failure in (False, True):
        with HARNESS.fixture(tmux) as (directory, socket):
            environment = HARNESS.clean_environment()
            environment["LIBTMUX_SOCKET_PATH"] = str(socket)
            environment["PATH"] = f"{Path(tmux).parent}:{environment.get('PATH', '')}"
            if failure:
                wrapper = directory / "tmux"
                wrapper.write_text("#!/bin/sh\nfor argument do\n"
                                   "  if [ \"$argument\" = list-panes ]; then\n"
                                   "    printf 'intentional lifecycle body failure\\n' >&2\n"
                                   "    exit 42\n  fi\ndone\n"
                                   f"exec {shlex.quote(tmux)} \"$@\"\n")
                wrapper.chmod(0o700)
                environment["PATH"] = f"{directory}:{environment['PATH']}"
            result = HARNESS.run([str(binaries / "lifecycle")], environment=environment, check=False)
            if failure:
                assert result.returncode != 0 and "intentional lifecycle body failure" in result.stderr, result
            else:
                assert result.returncode == 0 and "created and reused a pane" in result.stdout, result
            remaining = HARNESS.run([tmux, "-N", "-S", str(socket), "list-sessions", "-F", "#{session_name}"],
                                    environment=HARNESS.clean_environment())
            assert not remaining.stdout.strip(), remaining.stdout
        print(f"PASS unchanged lifecycle example {'failure' if failure else 'success'}; resources, daemon and files cleaned")
    with HARNESS.fixture(tmux) as (_directory, socket):
        environment = HARNESS.clean_environment()
        environment["LIBTMUX_SOCKET_PATH"] = str(socket)
        environment["PATH"] = f"{Path(tmux).parent}:{environment.get('PATH', '')}"
        HARNESS.run([tmux, "-N", "-S", str(socket), "new-session", "-d", "-s", "adoption-example"], environment=environment)
        result = HARNESS.run([str(binaries / "adopt_resources"), "adoption-example"], environment=environment)
        assert "adopted session, window and pane" in result.stdout, result
        remaining = HARNESS.run([tmux, "-N", "-S", str(socket), "list-sessions", "-F", "#{session_name}"], environment=environment)
        assert not remaining.stdout.strip(), remaining.stdout
    print("PASS ordinary session/window/pane adoption example; remote resources, daemon and files cleaned")
    with HARNESS.fixture(tmux) as (_directory, socket):
        environment = HARNESS.clean_environment()
        environment["PATH"] = f"{Path(tmux).parent}:{environment.get('PATH', '')}"
        result = HARNESS.run([str(binaries / "adopt_server"), str(socket)], environment=environment)
        assert "accepted daemon" in result.stdout, result
        alive = HARNESS.run([tmux, "-N", "-S", str(socket), "list-sessions"], environment=environment, check=False)
        assert alive.returncode != 0, "adopted daemon still answers"
    print("PASS explicit server adoption example; daemon termination observed before directory removal")
    assert os.environ == before
    print("PASS host environment preserved")


if __name__ == "__main__":
    main()
