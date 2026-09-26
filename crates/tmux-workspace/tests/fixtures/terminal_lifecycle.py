#!/usr/bin/env python3
"""Drive the native CLI through an owned Linux PTY and inspect kernel state."""
import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import sys
import tempfile
import termios
import time


def terminal_session():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def run_supervisor(argv):
    directory = Path.cwd()
    original = termios.tcgetattr(0)
    child = subprocess.Popen(argv, process_group=0)
    old_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTTOU})
    os.tcsetpgrp(0, child.pid)
    os.killpg(child.pid, signal.SIGCONT)
    (directory / "cli.pid").write_text(str(child.pid))
    try:
        deadline = time.monotonic() + 6
        while child.poll() is None and time.monotonic() < deadline:
            if (directory / "take-foreground").exists() and not (directory / "foreground-taken").exists():
                os.tcsetpgrp(0, os.getpgrp())
                (directory / "foreground-taken").touch()
            if (directory / "return-foreground").exists() and not (directory / "foreground-returned").exists():
                os.tcsetpgrp(0, child.pid)
                (directory / "foreground-returned").touch()
            time.sleep(0.005)
        if child.poll() is None:
            child.kill()
        status = child.wait(timeout=2)
        result = {
            "status": status,
            "foreground": os.tcgetpgrp(0),
            "expected_foreground": child.pid,
            "termios_restored": termios.tcgetattr(0) == original,
            "termios_before": repr(original),
            "termios_after": repr(termios.tcgetattr(0)),
        }
        report_tmp = directory / "supervisor.json.tmp"
        report_tmp.write_text(json.dumps(result))
        os.replace(report_tmp, directory / "supervisor.json")
        deadline = time.monotonic() + 3
        while not (directory / "ack").exists() and time.monotonic() < deadline:
            time.sleep(0.005)
    finally:
        os.tcsetpgrp(0, os.getpgrp())
        termios.tcsetattr(0, termios.TCSANOW, original)
        signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)


def process_state(pid):
    try:
        raw = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return {"state": raw[0], "start": raw[19], "group": int(raw[2])}
    except (FileNotFoundError, ProcessLookupError):
        return None


def terminal_descriptors(pid):
    result = []
    try:
        for fd in Path(f"/proc/{pid}/fd").iterdir():
            try:
                if os.readlink(fd).startswith("/dev/pts/"):
                    result.append(int(fd.name))
            except FileNotFoundError:
                pass
    except PermissionError:
        for _ in range(10):
            state = process_state(pid)
            if state is None or state["state"] == "Z":
                return None
            time.sleep(0.002)
        raise
    except (FileNotFoundError, ProcessLookupError):
        return None
    return sorted(result)


def case(binary, tmux, action, append, tostop, socket_name, fixture_root):
    directory = Path(tempfile.mkdtemp(prefix="terminal-", dir=fixture_root))
    socket = Path(socket_name)
    environment = dict(os.environ, HOME=str(directory), LIBTMUX_TEST_TMUX=tmux,
                       TERM="xterm-256color", NO_COLOR="1")
    environment.pop("TMUX", None)
    environment.pop("TMUX_PANE", None)
    def native(*argv):
        completed = subprocess.run([tmux, "-S", str(socket), *argv],
                                   env=environment, capture_output=True, timeout=3)
        if completed.returncode:
            raise RuntimeError((argv, completed.returncode, completed.stderr))
        return completed.stdout.decode().strip()
    native("-f", "/dev/null", "new-session", "-d", "-s", "keeper")
    keeper = native("list-panes", "-a", "-F", "#{pid}:#{session_id}:#{window_id}:#{pane_id}")
    assert keeper and keeper.count(":") == 3, keeper
    daemon_pid = int(keeper.split(":", 1)[0])
    daemon_fd = os.pidfd_open(daemon_pid)
    if append:
        native("new-session", "-d", "-s", "active")
        environment["TMUX_PANE"] = native("list-panes", "-t", "=active", "-F", "#{pane_id}")
        environment["TMUX"] = f"{socket},{daemon_pid},0"
    initial_active = native("list-panes", "-t", "=active", "-F", "#{pid}:#{session_id}:#{window_id}:#{pane_id}") if append else None
    script = directory / "before.sh"
    end = "exit 0" if action in ("success", "pipe-owner-exits") else "exit 7" if action == "failure" else "wait"
    script.write_text("#!/bin/sh\nstty -echo\n: > input-ready\nread -r value\n"
                      "[ \"$value\" = 'literal terminal input' ] || exit 8\n"
                      + ("sleep 30 &\nsleeper=$!\n" if action not in ("success", "failure") else "sleeper=$$\n")
                      + "printf 'SCRIPT_STDOUT\\n'\nprintf 'SCRIPT_STDERR\\n' >&2\n"
                      + "printf '%s %s\\n' \"$$\" \"$sleeper\" > ready.tmp\nmv ready.tmp ready\n"
                      + ("while [ ! -f read-again ]; do sleep 0.01; done\nread -r again\n" if action == "background-read" else "")
                      + end + "\n")
    config = {"session_name": "active", "before_script": f"sh {script}",
              "start_directory": str(directory), "windows": [{"panes": ["blank"]}]}
    (directory / "workspace.json").write_text(json.dumps(config))
    master, slave = pty.openpty()
    if tostop:
        attributes = termios.tcgetattr(slave)
        attributes[3] |= termios.TOSTOP
        termios.tcsetattr(slave, termios.TCSANOW, attributes)
    os.set_blocking(master, False)
    argv = [binary, "load", "--no-progress", "--color", "never", "-S", str(socket)]
    # `-d` now always wins over `--append`, so getting a borrowed session
    # here relies on `--append` alone to skip the attach step, same as `-d`
    # otherwise does for the owned-session case.
    argv += ["--append"] if append else ["-d"]
    argv += [str(directory / "workspace.json")]
    supervisor = subprocess.Popen([sys.executable, __file__, "--supervisor", *argv],
                                  cwd=directory, env=environment, stdin=slave,
                                  stdout=slave, stderr=slave, preexec_fn=terminal_session)
    os.close(slave)
    sent_input = False
    sent_signal = False
    stop_sent = False
    stop_observed = None
    background_observed = None
    background_phase = 0
    pids = []
    pidfds = []
    before = {}
    descriptors = {}
    retained_leader = None
    terminal_text = bytearray()
    report = None
    deadline = time.monotonic() + 8
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.005)[0]:
                try:
                    terminal_text.extend(os.read(master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
            if not sent_input and (directory / "input-ready").exists():
                os.write(master, b"literal terminal input\n")
                sent_input = True
            if not pids and (directory / "ready").exists():
                pids = list(set(map(int, (directory / "ready").read_text().split())))
                for pid in pids:
                    try:
                        pidfds.append((pid, os.pidfd_open(pid)))
                    except ProcessLookupError:
                        pass
                for pid in pids:
                    before[pid] = process_state(pid)
                    descriptors[pid] = terminal_descriptors(pid)
            if pids and action == "pipe-owner-exits" and retained_leader is None:
                for pid in pids:
                    state = process_state(pid)
                    if state and state["state"] == "Z":
                        retained_leader = {"pid": pid, **state}
            if pids and action in ("terminal-TSTP", "background-resume", "background-read") and not stop_sent:
                if action == "background-read":
                    os.kill(int((directory / "cli.pid").read_text()), signal.SIGSTOP)
                else:
                    os.write(master, b"\x1a")
                stop_sent = True
                stop_deadline = time.monotonic() + 1
            if stop_sent and stop_observed is None:
                cli_pid = int((directory / "cli.pid").read_text())
                state = process_state(cli_pid)
                if state and state["state"] == "T":
                    stop_observed = {"state": state, "foreground": os.tcgetpgrp(master),
                                     "echo": bool(termios.tcgetattr(master)[3] & termios.ECHO)}
                    if action in ("background-resume", "background-read"):
                        (directory / "take-foreground").touch()
                        background_phase = 1
                    else:
                        os.kill(cli_pid, signal.SIGCONT)
                    resume_deadline = time.monotonic() + 0.5
                elif time.monotonic() >= stop_deadline:
                    stop_observed = {"state": state, "failed_to_stop": True}
                    resume_deadline = time.monotonic()
            if background_phase == 1 and (directory / "foreground-taken").exists():
                if action == "background-read":
                    (directory / "read-again").touch()
                    if not any((state := process_state(pid)) and state["state"] == "T" for pid in pids):
                        continue
                os.kill(int((directory / "cli.pid").read_text()), signal.SIGCONT)
                background_phase = 2
                background_deadline = time.monotonic() + 0.5
            if background_phase == 2 and time.monotonic() >= background_deadline:
                cli_pid = int((directory / "cli.pid").read_text())
                background_observed = {"state": process_state(cli_pid),
                                       "foreground": os.tcgetpgrp(master),
                                       "expected_foreground": supervisor.pid}
                (directory / "return-foreground").touch()
                background_phase = 3
            if background_phase == 3 and (directory / "foreground-returned").exists():
                os.kill(int((directory / "cli.pid").read_text()), signal.SIGCONT)
                background_phase = 4
                resume_deadline = time.monotonic() + 0.5
            if stop_observed is not None and not sent_signal and time.monotonic() >= resume_deadline and (action not in ("background-resume", "background-read") or background_phase == 4):
                os.kill(int((directory / "cli.pid").read_text()), signal.SIGTERM)
                sent_signal = True
            if pids and not sent_signal and action in ("INT", "TERM", "terminal-INT"):
                cli_pid = int((directory / "cli.pid").read_text())
                if action == "terminal-INT":
                    os.write(master, b"\x03")
                else:
                    os.kill(cli_pid, getattr(signal, "SIG" + action))
                sent_signal = True
            if (directory / "supervisor.json").exists():
                report = json.loads((directory / "supervisor.json").read_text())
                break
        assert report is not None, {"directory": str(directory), "terminal": terminal_text.decode(errors="replace")}
        settle = time.monotonic() + 0.5
        while any((state := process_state(pid)) and state["state"] != "Z" for pid in pids) and time.monotonic() < settle:
            time.sleep(0.005)
        after = {pid: process_state(pid) for pid in pids}
        observed_keeper = native("list-panes", "-t", "=keeper", "-F", "#{pid}:#{session_id}:#{window_id}:#{pane_id}")
        assert observed_keeper == keeper, (keeper, observed_keeper)
        # A failing before_script removes the session it owns; an appended
        # (borrowed) one is never touched. Either way there is nothing named
        # "active" left to query when this run both owns the session and
        # fails its before_script.
        owned_failure = action == "failure" and not append
        active = None
        if not owned_failure:
            active = native("list-panes", "-t", "=active", "-F", "#{pid}:#{session_id}:#{window_id}:#{pane_id}")
            assert active and all(part for row in active.splitlines() for part in row.split(":")), active
            assert all(row.split(":", 1)[0] == str(daemon_pid) for row in active.splitlines()), active
            if append and action != "success":
                assert active == initial_active, (initial_active, active)
        else:
            try:
                native("list-panes", "-t", "=active")
                raise AssertionError("a failing before_script should remove the owned session")
            except RuntimeError:
                pass
        retained = None
        # Human mode reports an owned before_script failure as one plain
        # sentence, not the machine record; only an actual interruption
        # (INT/TERM/terminal-INT et al.) still retains "Retained state: ".
        if report["status"] != 0 and not owned_failure:
            lines = terminal_text.decode(errors="replace").splitlines()
            retained = json.loads(next(line[len("Retained state: "):] for line in lines if line.startswith("Retained state: ")))
            effects = retained["errors"][0]["effects"]
            assert effects["session_id"] == active.split(":")[1], (effects, active)
            assert effects["owned_session"] == (not append), effects
            assert effects["stage"] == "before-script", effects
        elif owned_failure:
            terminal_text_str = terminal_text.decode(errors="replace")
            assert "Retained state:" not in terminal_text_str, terminal_text_str
            assert "before_script" in terminal_text_str, terminal_text_str
        report.update(action=action, append=append, input_delivered=sent_input,
                      signal_sent=sent_signal, before=before, after=after,
                      stop_observed=stop_observed,
                      background_observed=background_observed, descriptors=descriptors,
                      retained_leader=retained_leader, tostop=tostop,
                      keeper=keeper, terminal=terminal_text.decode(errors="replace"),
                      active=active, retained=retained,
                      evidence_directory=str(directory))
        report["children_stopped"] = all(state is None or state["state"] == "Z" for state in after.values())
        report["pass"] = (sent_input and bool(pids) and report["termios_restored"]
                          and report["foreground"] == report["expected_foreground"]
                          and report["children_stopped"]
                          and all(fds in (None, [], [0]) for fds in descriptors.values())
                          and (action not in ("background-resume", "background-read") or (background_observed
                               and background_observed["state"] is not None
                               and background_observed["state"]["state"] == "T"
                               and background_observed["foreground"] == supervisor.pid))
                          and (action != "pipe-owner-exits" or retained_leader is not None)
                          and (action != "terminal-TSTP" or (stop_observed
                               and not stop_observed.get("failed_to_stop")
                               and stop_observed["foreground"] == report["expected_foreground"]
                               and stop_observed["echo"]))
                          and report["status"] == (0 if action in ("success", "pipe-owner-exits") else 1 if action == "failure" else 130))
        return report
    finally:
        for _, fd in pidfds:
            try:
                signal.pidfd_send_signal(fd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(fd)
        (directory / "ack").touch()
        try:
            supervisor.wait(timeout=4)
        except subprocess.TimeoutExpired:
            supervisor.kill()
            supervisor.wait(timeout=2)
        os.close(master)
        os.close(daemon_fd)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--supervisor":
        run_supervisor(sys.argv[2:])
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("binary")
        parser.add_argument("tmux")
        parser.add_argument("--action", default="TERM")
        parser.add_argument("--append", action="store_true")
        parser.add_argument("--tostop", action="store_true")
        parser.add_argument("--socket", required=True)
        parser.add_argument("--fixture-root", required=True)
        args = parser.parse_args()
        result = case(args.binary, args.tmux, args.action, args.append, args.tostop, args.socket, args.fixture_root)
        print(json.dumps(result, indent=2))
        sys.exit(0 if result["pass"] else 1)
