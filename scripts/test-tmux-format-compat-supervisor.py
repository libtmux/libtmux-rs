#!/usr/bin/env python3

"""Exercise the compatibility supervisor against hostile real processes."""

from __future__ import annotations

import contextlib
import ctypes
import dataclasses
import importlib.util
import os
import pathlib
import select
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import traceback
import types
import typing as t

# Test failures include the exact process identity at the assertion site.
# ruff: noqa: BLE001, EM101, EM102, TRY003


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
SUPERVISOR = SCRIPT_DIR / "tmux-format-compat-supervisor.py"
WRAPPER = SCRIPT_DIR / "test-tmux-format-compat.sh"
WAIT_TIMEOUT = 12.0
CASE_TIMEOUT = 20.0
CASE_CLEANUP_TIMEOUT = 6.0
TOPMOST_CLEANUP_TIMEOUT = 3.0
WATCHED_SIGNALS = (signal.SIGHUP, signal.SIGINT, signal.SIGTERM)
CASE_ROOT_PARENT_FD_ENV = "LIBTMUX_TFC_CASE_ROOT_PARENT_FD"
FORGED_ROOT_ENV = "TFC_FORGED_ROOT"
AFTER_ROOT_CREATE_CONTROL_ENV = "TFC_AFTER_ROOT_CREATE_CONTROL"
PAUSE_CASE_ROOT_ENV = "TFC_PAUSE_CASE_ROOT"
PAUSE_CASE_ROOT_CONTROL_ENV = "TFC_PAUSE_CASE_ROOT_CONTROL"
PAUSE_SAME_DEVICE_ROOT = "same-device-root"
PAUSE_UNTRUSTED_ROOT = "untrusted-root"
FRONTIER_TIMEOUT_CONTROL_ENV = "TFC_FRONTIER_TIMEOUT_CONTROL"
FRONTIER_MUTATION_HOLD_ENV = "TFC_FRONTIER_MUTATION_HOLD"
FINAL_CUTOVER_STOP_FD_ENV = "LIBTMUX_TFC_TEST_FINAL_CUTOVER_STOP_FD"
RUNNER_CUTOVER_STOP_FD_ENV = "LIBTMUX_TFC_TEST_RUNNER_CUTOVER_STOP_FD"

INTERNAL_CASE_MODE = "__libtmux_supervisor_test_case"
RUNNER_FAILURE_PROBE = "__runner_failure_probe"
RUNNER_TIMEOUT_PROBE = "__runner_timeout_probe"
RUNNER_INNER_FAILURE_PROBE = "__runner_inner_failure_probe"
RUNNER_INNER_TIMEOUT_PROBE = "__runner_inner_timeout_probe"
RUNNER_TOPMOST_FAILURE_PROBE = "__runner_topmost_failure_probe"
RUNNER_ROOT_REPLACEMENT_PROBE = "__runner_root_replacement_probe"
RUNNER_FORGED_ROOT_PROBE = "__runner_forged_root_probe"
RUNNER_CLEAN_EXIT_PROBE = "__runner_clean_exit_probe"
RUNNER_DANGLING_ROOT_PROBE = "__runner_dangling_root_probe"
SYS_CLONE3 = 435
WAIT_ALL_CHILDREN = 0x40000000
RENAME_EXCHANGE = 0x2


class CloneArgs(ctypes.Structure):
    """Arguments for a raw Linux clone3 call."""

    _fields_ = [
        ("flags", ctypes.c_uint64),
        ("pidfd", ctypes.c_uint64),
        ("child_tid", ctypes.c_uint64),
        ("parent_tid", ctypes.c_uint64),
        ("exit_signal", ctypes.c_uint64),
        ("stack", ctypes.c_uint64),
        ("stack_size", ctypes.c_uint64),
        ("tls", ctypes.c_uint64),
        ("set_tid", ctypes.c_uint64),
        ("set_tid_size", ctypes.c_uint64),
        ("cgroup", ctypes.c_uint64),
    ]


class MountIdNamespace(t.Protocol):
    """Mutable mount-query seam exposed by the loaded supervisor module."""

    _mount_id: t.Callable[[int], int]


class OwnedBuildRoot(t.Protocol):
    """Descriptor-held build root used by exact test cleanup."""

    deleted: bool
    parent_fd: int
    name: str
    root_fd: int
    device: int
    inode: int
    mount_id: int
    parent_path: str

    @property
    def path(self) -> str:
        """Return the creation path for diagnostics and worker input."""

    def delete(self) -> None:
        """Delete the still-owned root."""

    def close(self) -> None:
        """Close ownership descriptors."""


class MutableBuildRoot(OwnedBuildRoot, t.Protocol):
    """Build-root surface used to inject a deterministic name race."""

    def _verify_root_name(self) -> None:
        """Verify the currently named root."""


class BuildRootFactory(t.Protocol):
    """Constructor shape of the dynamically loaded build-root owner."""

    def __call__(
        self,
        parent_fd: int,
        name: str,
        root_fd: int,
        device: int,
        inode: int,
        mount_id: int,
        parent_path: str = "/tmp",
    ) -> OwnedBuildRoot:
        """Construct a descriptor-held build-root owner."""

    def create(self, parent_fd: int | None = None) -> OwnedBuildRoot:
        """Create a root while taking ownership of an optional parent fd."""


class CleanupModule(t.Protocol):
    """Supervisor module surface used for descriptor-safe test cleanup."""

    _mount_id: t.Callable[[int], int]
    BuildRoot: BuildRootFactory


class RunnerSignalLatch(t.Protocol):
    """Signal-latch surface needed by the top-level containment owner."""

    first: int | None

    def install(self) -> None:
        """Install watched-signal handlers."""

    def restore(self) -> None:
        """Close the latch descriptors."""


class RunnerLineageOwner(t.Protocol):
    """Supervisor surface reused for adopted-child KILL closure."""

    def _kill_closure(self, deadline: float) -> bool:
        """Kill and reap every directly adopted lineage member."""

    def close_pidfds(self) -> None:
        """Close any retained process descriptors."""


class RunnerModule(CleanupModule, t.Protocol):
    """Supervisor module surface used by top-level containment."""

    SignalLatch: t.Callable[[], RunnerSignalLatch]
    Supervisor: t.Callable[
        [list[str], object | None, RunnerSignalLatch, str | None],
        RunnerLineageOwner,
    ]

    def _enable_and_verify_subreaper(self) -> None:
        """Enable subreaper ownership in the current runner process."""


WORKER_SOURCE = r"""#!/usr/bin/env bash
set -euo pipefail

root="$1"
printf '%s\n' "$root" > "$TFC_CONTROL/root"
if [[ -n ${LIBTMUX_TFC_TEST_FINAL_CUTOVER_STOP_FD+x} ]]; then
    : > "$TFC_CONTROL/final-cutover-hook-leaked"
fi
kill -STOP -- "$BASHPID"
printf 'worker stdout\n'
printf 'worker stderr\n' >&2

wait_forever() {
    while :; do
        read -r -t 1 _ || :
    done
}

case "$TFC_SCENARIO" in
    status37)
        : > "$TFC_CONTROL/ready"
        exit 37
        ;;
    signal)
        : > "$TFC_CONTROL/ready"
        wait_forever
        ;;
    second-signal)
        trap '' HUP INT TERM
        : > "$TFC_CONTROL/ready"
        wait_forever
        ;;
    hostile-tree | term-fork | kill-frontier)
        "$TFC_PYTHON" "$TFC_HELPER" "$TFC_SCENARIO" "$TFC_CONTROL" &
        : > "$TFC_CONTROL/worker-ready"
        wait_forever
        ;;
    replace-root)
        : > "$root/original-sentinel"
        mv -- "$root" "$root.moved"
        mkdir -- "$TFC_CONTROL/replacement-target"
        : > "$TFC_CONTROL/replacement-target/replacement-sentinel"
        ln -s -- "$TFC_CONTROL/replacement-target" "$root"
        : > "$TFC_CONTROL/ready"
        ;;
    replace-root-directory)
        : > "$root/original-sentinel"
        mv -- "$root" "$root.moved"
        mkdir -- "$root"
        : > "$root/replacement-sentinel"
        : > "$TFC_CONTROL/ready"
        ;;
    dangling-root)
        : > "$root/original-sentinel"
        mv -- "$root" "$root.moved"
        ln -s -- "$TFC_CONTROL/missing-target" "$root"
        : > "$TFC_CONTROL/ready"
        ;;
    retain-root)
        : > "$root/retained-sentinel"
        : > "$TFC_CONTROL/ready"
        ;;
    preflight)
        : > "$TFC_CONTROL/continued"
        : > "$TFC_CONTROL/ready"
        wait_forever
        ;;
    parallel)
        : > "$TFC_CONTROL/ready"
        while [[ ! -e "$TFC_CONTROL/release" ]]; do
            read -r -t 0.05 _ || :
        done
        ;;
    *)
        printf 'unknown test scenario\n' >&2
        exit 64
        ;;
esac
"""


HELPER_SOURCE = r"""#!/usr/bin/env python3
from __future__ import annotations

import ctypes
import os
import pathlib
import signal
import sys
import time


PR_GET_DUMPABLE = 3
PR_SET_DUMPABLE = 4
PR_SET_NAME = 15


def start_time(pid: int) -> int:
    data = pathlib.Path(f"/proc/{pid}/stat").read_bytes()
    command_end = data.rfind(b") ")
    if command_end < 0:
        raise RuntimeError("malformed proc stat")
    fields = data[command_end + 2 :].split()
    return int(fields[19])


def append_identity(control: pathlib.Path, label: str) -> None:
    line = f"{label} {os.getpid()} {start_time(os.getpid())}\n".encode()
    descriptor = os.open(
        control / "identities",
        os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_CLOEXEC,
        0o600,
    )
    try:
        os.write(descriptor, line)
    finally:
        os.close(descriptor)


def wait_forever() -> None:
    while True:
        signal.pause()


def harden_identity(control: pathlib.Path) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_NAME, ctypes.c_char_p(b"line\nname"), 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_NAME failed")
    os.environ.clear()
    if libc.prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_DUMPABLE failed")
    dumpable = libc.prctl(PR_GET_DUMPABLE, 0, 0, 0, 0)
    if dumpable != 0:
        raise RuntimeError(f"unexpected dumpable state: {dumpable}")
    name = pathlib.Path("/proc/self/comm").read_bytes()
    (control / "features").write_text(
        f"dumpable={dumpable}\nenviron={len(os.environ)}\ncomm={name.hex()}\n",
        encoding="utf-8",
    )


def hostile_tree(control: pathlib.Path) -> None:
    first = os.fork()
    if first != 0:
        os._exit(0)
    os.setsid()
    second = os.fork()
    if second != 0:
        os._exit(0)
    harden_identity(control)
    append_identity(control, "hostile-tree")
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    (control / "ready").touch()
    wait_forever()


def term_fork(control: pathlib.Path) -> None:
    append_identity(control, "term-parent")

    def escape(_signum: int, _frame: object) -> None:
        child = os.fork()
        if child == 0:
            os.setsid()
            harden_identity(control)
            append_identity(control, "term-escape")
            signal.signal(signal.SIGHUP, signal.SIG_IGN)
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            (control / "escaped").touch()
            wait_forever()
        os._exit(0)

    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, escape)
    (control / "ready").touch()
    wait_forever()


def kill_frontier(control: pathlib.Path, generation: int = 0) -> None:
    if generation == 0:
        null_fd = os.open(os.devnull, os.O_RDWR | os.O_CLOEXEC)
        try:
            os.dup2(null_fd, 1)
            os.dup2(null_fd, 2)
        finally:
            os.close(null_fd)
    append_identity(control, f"kill-frontier-{generation}")
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    if generation < 3:
        child = os.fork()
        if child == 0:
            kill_frontier(control, generation + 1)
            os._exit(0)
    else:
        (control / "ready").touch()
    wait_forever()


scenario = sys.argv[1]
control = pathlib.Path(sys.argv[2])
if scenario == "hostile-tree":
    hostile_tree(control)
elif scenario == "term-fork":
    term_fork(control)
elif scenario == "kill-frontier":
    kill_frontier(control)
else:
    raise SystemExit(64)
"""


RUNNER_PROBE_SOURCE = r"""from __future__ import annotations

import os
import pathlib
import signal
import sys


def start_time(pid: int) -> int:
    data = pathlib.Path(f"/proc/{pid}/stat").read_bytes()
    fields = data[data.rfind(b") ") + 2 :].split()
    return int(fields[19])


control = pathlib.Path(sys.argv[1])
os.setsid()
identity = f"runner-probe {os.getpid()} {start_time(os.getpid())}\n"
(control / "identities").write_text(identity, encoding="utf-8")
signal.signal(signal.SIGHUP, signal.SIG_IGN)
signal.signal(signal.SIGINT, signal.SIG_IGN)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
(control / "ready").touch()
while True:
    signal.pause()
"""


DIRECT_CHILD_HOLDER_SOURCE = r"""from __future__ import annotations

import signal


while True:
    signal.pause()
"""


REUSED_PID_PARENT_SOURCE = r"""from __future__ import annotations

import os
import pathlib
import signal
import sys


def start_time(pid: int) -> int:
    data = pathlib.Path(f"/proc/{pid}/stat").read_bytes()
    fields = data[data.rfind(b") ") + 2 :].split()
    return int(fields[19])


def append_identity(control: pathlib.Path, label: str) -> None:
    payload = f"{label} {os.getpid()} {start_time(os.getpid())}\n".encode()
    descriptor = os.open(
        control / "identities",
        os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_CLOEXEC,
        0o600,
    )
    try:
        os.write(descriptor, payload)
    finally:
        os.close(descriptor)


control = pathlib.Path(sys.argv[1])
gate_read, gate_write = os.pipe()
child = os.fork()
if child == 0:
    os.close(gate_read)
    append_identity(control, "reused-nested")
    os.write(gate_write, b"r")
    os.close(gate_write)
    while True:
        signal.pause()

os.close(gate_write)
if os.read(gate_read, 1) != b"r":
    raise RuntimeError("nested child did not become ready")
os.close(gate_read)
append_identity(control, "reused-parent")
(control / "child-pid").write_text(str(child), encoding="utf-8")
(control / "ready").touch()
while True:
    signal.pause()
"""


@dataclasses.dataclass(frozen=True)
class Fixture:
    """Paths used by one functional supervisor fixture."""

    directory: pathlib.Path
    worker: pathlib.Path
    helper: pathlib.Path
    bash: str

    def control(self, name: str = "control") -> pathlib.Path:
        """Create and return an out-of-tree control directory."""
        path = self.directory / name
        path.mkdir()
        return path


@dataclasses.dataclass(frozen=True)
class Outcome:
    """A completed supervisor invocation."""

    status: int
    stdout: str
    stderr: str
    elapsed: float
    timed_out: bool = False


def load_supervisor_module() -> types.ModuleType:
    """Load the supervisor so a kernel query can be replaced at its boundary."""
    module_name = "_libtmux_tmux_format_compat_supervisor"
    specification = importlib.util.spec_from_file_location(module_name, SUPERVISOR)
    if specification is None or specification.loader is None:
        raise AssertionError("could not load compatibility supervisor")
    module = importlib.util.module_from_spec(specification)
    sys.modules[module_name] = module
    try:
        specification.loader.exec_module(module)
    except BaseException:
        del sys.modules[module_name]
        raise
    return module


def create_owned_root(
    module: CleanupModule,
    *,
    require_parent: bool,
) -> OwnedBuildRoot:
    """Create a root beneath a duplicated enclosing ownership descriptor."""
    raw_parent_fd = os.environ.get(CASE_ROOT_PARENT_FD_ENV)
    if raw_parent_fd is None:
        if require_parent:
            raise AssertionError("nested build root lacks an owned parent")
        return module.BuildRoot.create()
    try:
        inherited_parent_fd = int(raw_parent_fd)
    except ValueError as error:
        raise AssertionError("invalid nested-root parent descriptor") from error
    if inherited_parent_fd <= 2:
        raise AssertionError("invalid nested-root parent descriptor")
    return module.BuildRoot.create(os.dup(inherited_parent_fd))


def create_named_owned_root(
    module: CleanupModule,
    parent_fd: int,
    parent_path: str,
    name: str,
) -> OwnedBuildRoot:
    """Create and hold one exact named root beneath an existing owner."""
    owned_parent_fd = os.dup(parent_fd)
    root_fd: int | None = None
    created = False
    try:
        os.mkdir(name, mode=0o700, dir_fd=owned_parent_fd)
        created = True
        root_fd = os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
            dir_fd=owned_parent_fd,
        )
        opened = os.fstat(root_fd)
        owner = module.BuildRoot(
            owned_parent_fd,
            name,
            root_fd,
            opened.st_dev,
            opened.st_ino,
            module._mount_id(root_fd),
            parent_path,
        )
        owned_parent_fd = -1
        root_fd = None
        return owner
    finally:
        if root_fd is not None:
            os.close(root_fd)
        if created and owned_parent_fd >= 0:
            os.rmdir(name, dir_fd=owned_parent_fd)
        if owned_parent_fd >= 0:
            os.close(owned_parent_fd)


def rename_exchange(
    first_parent_fd: int,
    first_name: str,
    second_parent_fd: int,
    second_name: str,
) -> None:
    """Atomically exchange two test-owned names under held parents."""
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = libc.renameat2
    renameat2.argtypes = (
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    )
    renameat2.restype = ctypes.c_int
    result = renameat2(
        first_parent_fd,
        os.fsencode(first_name),
        second_parent_fd,
        os.fsencode(second_name),
        RENAME_EXCHANGE,
    )
    if result != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number))


def raw_clone_without_exit_signal() -> tuple[int, int]:
    """Create a direct clone child which exits only after its gate is released."""
    gate_read_fd, gate_write_fd = os.pipe2(os.O_CLOEXEC)
    libc = ctypes.CDLL(None, use_errno=True)
    libc.syscall.restype = ctypes.c_long
    result = libc.syscall(
        SYS_CLONE3,
        ctypes.byref(CloneArgs(exit_signal=0)),
        ctypes.sizeof(CloneArgs),
    )
    if result < 0:
        error_number = ctypes.get_errno()
        os.close(gate_read_fd)
        os.close(gate_write_fd)
        raise OSError(error_number, os.strerror(error_number))
    if result == 0:
        try:
            os.close(gate_write_fd)
            os.read(gate_read_fd, 1)
        finally:
            os._exit(0)

    os.close(gate_read_fd)
    return result, gate_write_fd


@contextlib.contextmanager
def fixture() -> t.Iterator[Fixture]:
    """Create real Bash and Python descendants for one test."""
    with tempfile.TemporaryDirectory(prefix="libtmux-supervisor-test-") as raw:
        directory = pathlib.Path(raw)
        worker = directory / "worker.sh"
        helper = directory / "helper.py"
        worker.write_text(WORKER_SOURCE, encoding="utf-8")
        helper.write_text(HELPER_SOURCE, encoding="utf-8")
        worker.chmod(0o700)
        helper.chmod(0o700)
        bash = shutil.which("bash")
        if bash is None:
            raise AssertionError("bash is unavailable")
        yield Fixture(directory, worker, helper, os.path.realpath(bash))


def supervisor_process(
    case: Fixture,
    control: pathlib.Path,
    scenario: str,
    *,
    extra_environment: dict[str, str] | None = None,
    extra_pass_fds: tuple[int, ...] = (),
    fault: str | None = None,
) -> subprocess.Popen[str]:
    """Start the real supervisor with the synthetic Bash worker."""
    environment = os.environ.copy()
    environment.update(
        {
            "TFC_CONTROL": os.fspath(control),
            "TFC_HELPER": os.fspath(case.helper),
            "TFC_PYTHON": sys.executable,
            "TFC_SCENARIO": scenario,
        }
    )
    if extra_environment is not None:
        environment.update(extra_environment)
    command = [sys.executable, os.fspath(SUPERVISOR)]
    if fault is not None:
        command.append(f"--test-fault={fault}")
    pass_fds = list(extra_pass_fds)
    parent_fd: int | None = None
    raw_parent_fd = environment.pop(CASE_ROOT_PARENT_FD_ENV, None)
    if raw_parent_fd is not None:
        inherited_parent_fd = int(raw_parent_fd)
        if inherited_parent_fd <= 2:
            raise AssertionError("invalid nested-root parent descriptor")
        parent_fd = os.dup(inherited_parent_fd)
        command.append(f"--root-parent-fd={parent_fd}")
        pass_fds.append(parent_fd)
    command.extend(["--", case.bash, os.fspath(case.worker)])
    try:
        return subprocess.Popen(
            command,
            env=environment,
            pass_fds=tuple(pass_fds),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    finally:
        if parent_fd is not None:
            os.close(parent_fd)


def wait_for_path(
    process: subprocess.Popen[str], path: pathlib.Path, timeout: float = 5.0
) -> None:
    """Wait for a fixture milestone while checking early supervisor exit."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        status = process.poll()
        if status is not None:
            stdout, stderr = process.communicate()
            raise AssertionError(
                f"supervisor exited {status} before {path.name}; "
                f"stdout={stdout!r} stderr={stderr!r}"
            )
        time.sleep(0.01)
    raise AssertionError(f"timed out waiting for {path.name}; pid={process.pid}")


def finish(process: subprocess.Popen[str], started: float) -> Outcome:
    """Collect one bounded supervisor result."""
    try:
        stdout, stderr = process.communicate(timeout=WAIT_TIMEOUT)
    except subprocess.TimeoutExpired as error:
        raise AssertionError(f"supervisor did not exit; pid={process.pid}") from error
    return Outcome(process.returncode, stdout, stderr, time.monotonic() - started)


def run_to_completion(
    case: Fixture,
    control: pathlib.Path,
    scenario: str,
    *,
    fault: str | None = None,
) -> Outcome:
    """Run a worker scenario without sending an external signal."""
    started = time.monotonic()
    process = supervisor_process(case, control, scenario, fault=fault)
    return finish(process, started)


def signal_and_finish(
    case: Fixture,
    control: pathlib.Path,
    scenario: str,
    first_signal: int,
    second_signal: int | None = None,
) -> Outcome:
    """Signal a ready supervisor and collect its bounded result."""
    process = supervisor_process(case, control, scenario)
    wait_for_path(process, control / "ready")
    started = time.monotonic()
    os.kill(process.pid, first_signal)
    if second_signal is not None:
        time.sleep(0.1)
        os.kill(process.pid, second_signal)
    return finish(process, started)


def build_root(control: pathlib.Path) -> pathlib.Path:
    """Read the worker's supervisor-owned build root."""
    return pathlib.Path((control / "root").read_text(encoding="utf-8").strip())


def root_entry_exists(path: pathlib.Path) -> bool:
    """Return whether a root name exists without following a final symlink."""
    try:
        path.lstat()
    except FileNotFoundError:
        return False
    return True


def pause_after_case_root(label: str, root: pathlib.Path) -> None:
    """Pause a selected case after allocation until outer hard containment."""
    if os.environ.get(PAUSE_CASE_ROOT_ENV) != label:
        return
    control = pathlib.Path(os.environ[PAUSE_CASE_ROOT_CONTROL_ENV])
    (control / "allocated-root").write_text(os.fspath(root), encoding="utf-8")
    for signal_number in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(signal_number, signal.SIG_IGN)
    (control / "ready").touch()
    while True:
        signal.pause()


def proc_start_time(pid: int) -> int | None:
    """Return a live process start time without trusting its comm field."""
    try:
        data = pathlib.Path(f"/proc/{pid}/stat").read_bytes()
    except FileNotFoundError:
        return None
    command_end = data.rfind(b") ")
    if command_end < 0:
        raise AssertionError(f"malformed stat for residue pid={pid}")
    fields = data[command_end + 2 :].split()
    return int(fields[19])


def require_process_start_time(process: subprocess.Popen[str]) -> int:
    """Capture the exact identity of one newly spawned direct child."""
    start_time = proc_start_time(process.pid)
    if start_time is None:
        raise AssertionError(
            f"process vanished before identity capture: pid={process.pid}"
        )
    return start_time


def proc_task_states(pid: int, expected_start: int) -> dict[int, str]:
    """Read all task states while authenticating the process identity."""
    if proc_start_time(pid) != expected_start:
        raise AssertionError(
            f"process identity changed: pid={pid} start={expected_start}"
        )
    states: dict[int, str] = {}
    for task in pathlib.Path(f"/proc/{pid}/task").iterdir():
        if not task.name.isdigit():
            continue
        data = (task / "stat").read_bytes()
        command_end = data.rfind(b") ")
        if command_end < 0:
            raise AssertionError(f"malformed task stat: pid={pid} tid={task.name}")
        fields = data[command_end + 2 :].split()
        states[int(task.name)] = fields[0].decode("ascii")
    if proc_start_time(pid) != expected_start:
        raise AssertionError(
            f"process identity changed: pid={pid} start={expected_start}"
        )
    return states


def wait_for_exact_process_stopped(
    process: subprocess.Popen[str],
    expected_start: int,
    timeout: float = 2.0,
) -> None:
    """Wait for a stable all-task stop of one authenticated process."""
    deadline = time.monotonic() + timeout
    latest: dict[int, str] = {}
    while time.monotonic() < deadline:
        try:
            first = proc_task_states(process.pid, expected_start)
            if first and all(state in {"T", "t"} for state in first.values()):
                second = proc_task_states(process.pid, expected_start)
                if first == second:
                    return
            latest = first
        except FileNotFoundError:
            if process.poll() is not None:
                raise AssertionError(
                    f"process exited before stop: pid={process.pid} "
                    f"start={expected_start}"
                ) from None
        time.sleep(0.01)
    raise AssertionError(
        f"process did not stop: pid={process.pid} start={expected_start} "
        f"states={latest}"
    )


def wait_for_hook_marker(
    process: subprocess.Popen[str],
    hook_read_fd: int,
    timeout: float = 2.0,
) -> None:
    """Wait for one post-mask hook byte without guessing at process timing."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        readable, _, _ = select.select(
            [hook_read_fd],
            [],
            [],
            min(0.05, deadline - time.monotonic()),
        )
        if readable:
            marker = os.read(hook_read_fd, 1)
            if marker == b"1":
                return
            raise AssertionError(
                f"post-mask hook closed without marker: pid={process.pid}"
            )
        if process.poll() is not None:
            raise AssertionError(
                f"process exited before post-mask hook: pid={process.pid}"
            )
    raise AssertionError(f"post-mask hook did not run: pid={process.pid}")


def assert_hook_capability_consumed(pid: int, hook_fd: int) -> None:
    """Require the stopped process to have closed its one-shot hook fd."""
    try:
        pathlib.Path(f"/proc/{pid}/fd/{hook_fd}").lstat()
    except FileNotFoundError:
        return
    raise AssertionError(f"post-mask hook descriptor remains open: pid={pid}")


def proc_signal_mask(pid: int, field: str) -> int:
    """Read one hexadecimal signal-mask field for an exact live process."""
    for line in (
        pathlib.Path(f"/proc/{pid}/status").read_text(encoding="ascii").splitlines()
    ):
        label, separator, raw_mask = line.partition(":")
        if separator and label == field:
            return int(raw_mask.strip(), 16)
    raise AssertionError(f"missing {field} for pid={pid}")


def signal_bit(signal_number: int) -> int:
    """Return the procfs bit corresponding to one signal number."""
    return 1 << (signal_number - 1)


def assert_signal_blocked(pid: int, signal_number: int) -> None:
    """Require the watched signal to be blocked at the injected stop."""
    blocked = proc_signal_mask(pid, "SigBlk")
    assert blocked & signal_bit(signal_number), (
        f"signal was not blocked: pid={pid} signal={signal_number} mask={blocked:x}"
    )


def wait_for_signal_pending(
    process: subprocess.Popen[str],
    signal_number: int,
    timeout: float = 2.0,
) -> None:
    """Wait until procfs proves a process- or thread-pending signal."""
    bit = signal_bit(signal_number)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        pending = proc_signal_mask(process.pid, "SigPnd")
        pending |= proc_signal_mask(process.pid, "ShdPnd")
        if pending & bit:
            return
        if process.poll() is not None:
            raise AssertionError(
                f"process exited before signal became pending: pid={process.pid}"
            )
        time.sleep(0.01)
    raise AssertionError(
        f"signal did not become pending: pid={process.pid} signal={signal_number}"
    )


def identities(control: pathlib.Path) -> list[tuple[str, int, int]]:
    """Read exact identities recorded by adversarial descendants."""
    path = control / "identities"
    if not path.exists():
        return []
    result = []
    for line in path.read_text(encoding="utf-8").splitlines():
        label, raw_pid, raw_start = line.split()
        result.append((label, int(raw_pid), int(raw_start)))
    return result


def live_identity_labels(control: pathlib.Path, prefix: str) -> set[str]:
    """Return matching labels whose recorded PID/start identity is still live."""
    return {
        label
        for label, pid, expected_start in identities(control)
        if label.startswith(prefix) and proc_start_time(pid) == expected_start
    }


def assert_identities_gone(control: pathlib.Path) -> None:
    """Fail with exact identities if an original descendant survives."""
    residue = []
    for label, pid, expected_start in identities(control):
        actual_start = proc_start_time(pid)
        if actual_start == expected_start:
            residue.append(f"{label}:pid={pid}:start={expected_start}")
    if residue:
        raise AssertionError("live process residue: " + ", ".join(residue))


def append_identity_record(
    control: pathlib.Path,
    label: str,
    pid: int,
    start_time: int,
) -> None:
    """Append one exact identity to the shared emergency-cleanup ledger."""
    payload = f"{label} {pid} {start_time}\n".encode()
    descriptor = os.open(
        control / "identities",
        os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_CLOEXEC,
        0o600,
    )
    try:
        os.write(descriptor, payload)
    finally:
        os.close(descriptor)


def record_process_identity(
    control: pathlib.Path,
    label: str,
    process: subprocess.Popen[str],
) -> None:
    """Record the exact live identity of one nested supervisor."""
    start_time = proc_start_time(process.pid)
    if start_time is None:
        raise AssertionError(
            f"process vanished before identity capture: pid={process.pid}"
        )
    append_identity_record(control, label, process.pid, start_time)


def copy_identity_records(source: pathlib.Path, destination: pathlib.Path) -> None:
    """Copy exact nested identities into an outer cleanup ledger."""
    for label, pid, start_time in identities(source):
        append_identity_record(destination, label, pid, start_time)


def kill_recorded_identities(control: pathlib.Path) -> None:
    """SIGKILL only still-matching recorded identities and await their exit."""
    pidfds: list[tuple[str, int, int, int]] = []
    try:
        for label, pid, expected_start in identities(control):
            if pid == os.getpid() or proc_start_time(pid) != expected_start:
                continue
            try:
                pidfd = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            if proc_start_time(pid) != expected_start:
                os.close(pidfd)
                continue
            try:
                signal.pidfd_send_signal(pidfd, signal.SIGKILL)
            except ProcessLookupError:
                os.close(pidfd)
                continue
            pidfds.append((label, pid, expected_start, pidfd))

        deadline = time.monotonic() + 3.0
        pending = pidfds
        while pending and time.monotonic() < deadline:
            pending = [
                identity
                for identity in pending
                if not select.select([identity[3]], [], [], 0)[0]
            ]
            if pending:
                time.sleep(0.01)
        if pending:
            residue = ", ".join(
                f"{label}:pid={pid}:start={start_time}"
                for label, pid, start_time, _pidfd in pending
            )
            raise AssertionError("emergency cleanup could not stop: " + residue)
    finally:
        for _label, _pid, _start_time, pidfd in pidfds:
            os.close(pidfd)


def cleanup_owned_root(owner: OwnedBuildRoot) -> str | None:
    """Delete only the still-named creation identity held by one owner."""
    try:
        held = os.fstat(owner.root_fd)
        if owner.deleted or held.st_nlink == 0:
            return None
        try:
            owner.delete()
        except Exception as error:
            path = pathlib.Path(owner.parent_path) / owner.name
            return f"retained creation identity: path={path} error={error}"
        return None
    finally:
        owner.close()


def touch_owned_root(owner: OwnedBuildRoot, name: str) -> None:
    """Create one test sentinel relative to a held root descriptor."""
    descriptor = os.open(
        name,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC,
        0o600,
        dir_fd=owner.root_fd,
    )
    os.close(descriptor)


def owned_root_has_entry(owner: OwnedBuildRoot, name: str) -> bool:
    """Check one sentinel relative to its creation-time root descriptor."""
    try:
        os.stat(name, dir_fd=owner.root_fd, follow_symlinks=False)
    except FileNotFoundError:
        return False
    return True


def cleanup_exact_owners(*owners: OwnedBuildRoot) -> None:
    """Delete only explicitly supplied creation-time owners."""
    failures = [
        failure
        for owner in owners
        if (failure := cleanup_owned_root(owner)) is not None
    ]
    if failures:
        raise AssertionError("exact owner cleanup failed: " + "; ".join(failures))


def spawn_runner_probe(control: pathlib.Path) -> None:
    """Start a setsid descendant which only outer containment may remove."""
    process = subprocess.Popen(
        [sys.executable, "-c", RUNNER_PROBE_SOURCE, os.fspath(control)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline:
        if (control / "ready").exists():
            return
        status = process.poll()
        if status is not None:
            process.wait()
            raise AssertionError(f"runner probe exited before ready: status={status}")
        time.sleep(0.01)
    raise AssertionError(f"runner probe did not become ready: pid={process.pid}")


def spawn_direct_child_holder(control: pathlib.Path) -> subprocess.Popen[str]:
    """Start and record a direct child that only top-level closure owns."""
    process = subprocess.Popen(
        [sys.executable, "-c", DIRECT_CHILD_HOLDER_SOURCE],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    record_process_identity(control, "direct-child-holder", process)
    return process


def spawn_nested_top_runner(
    name: str,
    control: pathlib.Path,
    extra_environment: dict[str, str] | None = None,
    extra_pass_fds: tuple[int, ...] = (),
) -> subprocess.Popen[str]:
    """Start a top-level runner beneath the current creation-time owner."""
    raw_parent_fd = os.environ.get(CASE_ROOT_PARENT_FD_ENV)
    if raw_parent_fd is None:
        raise AssertionError("nested top runner lacks its enclosing owner")
    inherited_parent_fd = int(raw_parent_fd)
    if inherited_parent_fd <= 2:
        raise AssertionError("invalid nested top-runner parent descriptor")
    child_parent_fd = os.dup(inherited_parent_fd)
    environment = os.environ.copy()
    if extra_environment is not None:
        environment.update(extra_environment)
    environment[CASE_ROOT_PARENT_FD_ENV] = str(child_parent_fd)
    environment["TFC_RUNNER_CONTROL"] = os.fspath(control)
    try:
        return subprocess.Popen(
            [sys.executable, os.fspath(pathlib.Path(__file__).resolve()), name],
            env=environment,
            pass_fds=(child_parent_fd, *extra_pass_fds),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    finally:
        os.close(child_parent_fd)


def runner_failure_probe(build_root_path: pathlib.Path) -> None:
    """Leave a hostile descendant behind while the case fails."""
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    (control / "outer-root").write_text(
        os.fspath(build_root_path),
        encoding="utf-8",
    )
    spawn_runner_probe(control)
    raise AssertionError("injected runner case failure")


def runner_timeout_probe(build_root_path: pathlib.Path) -> None:
    """Leave a hostile descendant behind while the case exceeds its deadline."""
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    (control / "outer-root").write_text(
        os.fspath(build_root_path),
        encoding="utf-8",
    )
    spawn_runner_probe(control)
    while True:
        signal.pause()


def record_nested_supervisor(
    destination: pathlib.Path,
    source: pathlib.Path,
    process: subprocess.Popen[str],
) -> None:
    """Record a nested supervisor, its direct children, and fixture identities."""
    record_process_identity(destination, "inner-supervisor", process)
    children: set[int] = set()
    for task in pathlib.Path(f"/proc/{process.pid}/task").iterdir():
        if not task.name.isdigit():
            continue
        payload = (task / "children").read_bytes()
        children.update(int(raw_pid) for raw_pid in payload.split())
    for child_pid in sorted(children):
        start_time = proc_start_time(child_pid)
        if start_time is not None:
            append_identity_record(
                destination,
                "inner-direct",
                child_pid,
                start_time,
            )
    copy_identity_records(source, destination)


def start_retained_nested_supervisor(
    case: Fixture,
    control: pathlib.Path,
    destination: pathlib.Path,
) -> subprocess.Popen[str]:
    """Start and publish one nested real supervisor that will retain its root."""
    process = supervisor_process(
        case,
        control,
        "hostile-tree",
        fault="proc-children",
    )
    wait_for_path(process, control / "ready")
    inner_root = build_root(control)
    (destination / "inner-root").write_text(
        os.fspath(inner_root),
        encoding="utf-8",
    )
    record_nested_supervisor(destination, control, process)
    return process


def runner_inner_failure_probe(build_root_path: pathlib.Path) -> None:
    """Fail while a nested real supervisor and its retained root are live."""
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    (control / "outer-root").write_text(
        os.fspath(build_root_path),
        encoding="utf-8",
    )
    with fixture() as case:
        inner_control = case.control()
        start_retained_nested_supervisor(case, inner_control, control)
        raise AssertionError("injected nested supervisor assertion failure")


def runner_inner_timeout_probe(build_root_path: pathlib.Path) -> None:
    """Time out while a nested real supervisor and retained root are live."""
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    (control / "outer-root").write_text(
        os.fspath(build_root_path),
        encoding="utf-8",
    )

    def interrupt_probe(_signal_number: int, _frame: types.FrameType | None) -> None:
        raise RuntimeError("nested timeout probe interrupted")

    previous = signal.signal(signal.SIGTERM, interrupt_probe)
    try:
        with fixture() as case:
            inner_control = case.control()
            start_retained_nested_supervisor(case, inner_control, control)
            while True:
                signal.pause()
    finally:
        signal.signal(signal.SIGTERM, previous)


def stop_parent_via_pidfd() -> None:
    """Stop the exact current parent without using a reusable raw PID signal."""
    parent_pid = os.getppid()
    parent_start = proc_start_time(parent_pid)
    if parent_start is None:
        raise AssertionError("outer supervisor vanished before forced stop")
    parent_pidfd = os.pidfd_open(parent_pid)
    try:
        if proc_start_time(parent_pid) != parent_start:
            raise AssertionError("outer supervisor identity changed before stop")
        signal.pidfd_send_signal(parent_pidfd, signal.SIGSTOP)
    finally:
        os.close(parent_pidfd)


def runner_topmost_failure_probe(build_root_path: pathlib.Path) -> None:
    """Stop the outer supervisor while nested processes and roots are live."""
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    (control / "outer-root").write_text(
        os.fspath(build_root_path),
        encoding="utf-8",
    )
    own_start = proc_start_time(os.getpid())
    if own_start is None:
        raise AssertionError("topmost probe identity is unavailable")
    append_identity_record(control, "topmost-worker", os.getpid(), own_start)
    with fixture() as case:
        inner_control = case.control()
        start_retained_nested_supervisor(case, inner_control, control)
        stop_parent_via_pidfd()
        while True:
            signal.pause()


def runner_root_replacement_probe(build_root_path: pathlib.Path) -> None:
    """Replace the parent-owned container before final cleanup."""
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    root = build_root_path.parent
    moved = pathlib.Path(f"{root}.moved")
    (root / "original-sentinel").touch()
    root.rename(moved)
    root.mkdir()
    (root / "replacement-sentinel").touch()
    (control / "inner-root").write_text(os.fspath(root), encoding="utf-8")
    (control / "inner-root-moved").write_text(
        os.fspath(moved),
        encoding="utf-8",
    )


def runner_forged_root_probe(build_root_path: pathlib.Path) -> None:
    """Verify the worker has no root-cleanup authorization capability."""
    del build_root_path
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    if "LIBTMUX_TFC_ROOT_OWNER_FD" in os.environ:
        raise AssertionError("worker inherited a root-cleanup capability")
    (control / "report-capability-absent").touch()


def runner_clean_exit_probe(build_root_path: pathlib.Path) -> None:
    """Record the outer root and return without spawning descendants."""
    if RUNNER_CUTOVER_STOP_FD_ENV in os.environ:
        raise AssertionError("runner cutover hook leaked to the case")
    control = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    (control / "outer-root").write_text(
        os.fspath(build_root_path),
        encoding="utf-8",
    )


def runner_dangling_root_probe(build_root_path: pathlib.Path) -> None:
    """Expose the following root-absence assertion to dangling residue."""
    del build_root_path
    destination = pathlib.Path(os.environ["TFC_RUNNER_CONTROL"])
    with fixture() as case:
        control = case.control()
        outcome = run_to_completion(case, control, "dangling-root")
        root = build_root(control)
        (destination / "dangling-root").write_text(
            os.fspath(root),
            encoding="utf-8",
        )
        assert outcome.status == 1, outcome
        assert not root_entry_exists(root), "supervised root entry remains"


def test_status_37_and_streams() -> None:
    """A nonzero worker status and both output streams pass through."""
    with fixture() as case:
        control = case.control()
        outcome = run_to_completion(case, control, "status37")
        assert outcome.status == 37, outcome
        root = build_root(control)
        assert outcome.stdout == "worker stdout\n", outcome
        assert outcome.stderr == "worker stderr\n", outcome
        assert not root_entry_exists(root), root


def test_wrapper_skips_python_without_pidfd() -> None:
    """The wrapper selects the first python3 with working pidfd primitives."""
    with tempfile.TemporaryDirectory(prefix="libtmux-wrapper-python-") as raw:
        directory = pathlib.Path(raw)
        incompatible_dir = directory / "incompatible"
        capable_dir = directory / "capable"
        incompatible_dir.mkdir()
        capable_dir.mkdir()
        wrapper = directory / WRAPPER.name
        supervisor = directory / SUPERVISOR.name
        control = directory / "control"
        control.mkdir()
        shutil.copy2(WRAPPER, wrapper)

        incompatible = incompatible_dir / "python3"
        incompatible.write_text(
            """#!/usr/bin/env bash
if [[ "${1:-}" == "-c" ]]; then
    printf 'probe\n' >> "$TFC_CONTROL/incompatible-probes"
    exit 1
fi
: > "$TFC_CONTROL/wrong-python"
""",
            encoding="utf-8",
        )
        incompatible.chmod(0o700)

        capable = capable_dir / "python3"
        capable.write_text(
            f"""#!/usr/bin/env bash
export TFC_SELECTED_PYTHON=capable
exec {shlex.quote(os.path.realpath(sys.executable))} "$@"
""",
            encoding="utf-8",
        )
        capable.chmod(0o700)

        supervisor.write_text(
            """from __future__ import annotations

import os
import pathlib

control = pathlib.Path(os.environ["TFC_CONTROL"])
(control / "selected-python").write_text(
    os.environ.get("TFC_SELECTED_PYTHON", "missing"),
    encoding="utf-8",
)
""",
            encoding="utf-8",
        )

        environment = os.environ.copy()
        environment["PATH"] = os.pathsep.join(
            [
                os.fspath(incompatible_dir),
                os.fspath(incompatible_dir),
                os.fspath(capable_dir),
                environment["PATH"],
            ]
        )
        environment["TFC_CONTROL"] = os.fspath(control)
        bash = shutil.which("bash")
        assert bash is not None
        outcome = subprocess.run(
            [os.path.realpath(bash), os.fspath(wrapper)],
            cwd=directory,
            env=environment,
            capture_output=True,
            text=True,
            timeout=5.0,
            check=False,
        )
        assert outcome.returncode == 0, outcome
        assert not (control / "wrong-python").exists(), outcome
        assert (control / "incompatible-probes").read_text(
            encoding="utf-8"
        ).splitlines() == ["probe"]
        assert (control / "selected-python").read_text(encoding="utf-8") == "capable"


def test_hup_int_term_statuses() -> None:
    """Each supported first signal controls the supervisor exit status."""
    for signal_number in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        with fixture() as case:
            control = case.control()
            outcome = signal_and_finish(case, control, "signal", signal_number)
            root = build_root(control)
            assert outcome.status == 128 + signal_number, outcome
            assert not root_entry_exists(root), root
            assert_identities_gone(control)


def test_second_signal_accelerates_without_replacing_status() -> None:
    """A later signal accelerates cleanup but the first signal remains latched."""
    with fixture() as case:
        control = case.control()
        outcome = signal_and_finish(
            case,
            control,
            "second-signal",
            signal.SIGTERM,
            signal.SIGINT,
        )
        root = build_root(control)
        assert outcome.status == 128 + signal.SIGTERM, outcome
        assert outcome.elapsed < 0.9, outcome
        assert not root_entry_exists(root), root


def test_late_first_signal_replaces_the_worker_status_at_cutover() -> None:
    """A first signal after worker-status selection remains authoritative."""
    with fixture() as case:
        control = case.control()
        hook_read_fd, hook_write_fd = os.pipe2(os.O_CLOEXEC)
        hook_target_fd = hook_write_fd
        process: subprocess.Popen[str] | None = None
        pidfd: int | None = None
        try:
            process = supervisor_process(
                case,
                control,
                "status37",
                extra_environment={
                    FINAL_CUTOVER_STOP_FD_ENV: str(hook_write_fd),
                },
                extra_pass_fds=(hook_write_fd,),
            )
            os.close(hook_write_fd)
            hook_write_fd = -1
            expected_start = require_process_start_time(process)
            pidfd = os.pidfd_open(process.pid)
            wait_for_hook_marker(process, hook_read_fd)
            wait_for_exact_process_stopped(process, expected_start)
            assert_hook_capability_consumed(process.pid, hook_target_fd)
            assert_signal_blocked(process.pid, signal.SIGTERM)
            signal.pidfd_send_signal(pidfd, signal.SIGTERM)
            wait_for_signal_pending(process, signal.SIGTERM)
            wait_for_exact_process_stopped(process, expected_start)
            signal.pidfd_send_signal(pidfd, signal.SIGCONT)
            outcome = finish(process, time.monotonic())
            root = build_root(control)
            assert outcome.status == 128 + signal.SIGTERM, outcome
            assert not (control / "final-cutover-hook-leaked").exists(), control
            assert not root_entry_exists(root), root
        finally:
            if process is not None and process.poll() is None:
                if pidfd is not None:
                    with contextlib.suppress(ProcessLookupError):
                        signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                else:
                    process.kill()
                process.communicate(timeout=TOPMOST_CLEANUP_TIMEOUT)
            if pidfd is not None:
                os.close(pidfd)
            os.close(hook_read_fd)
            if hook_write_fd >= 0:
                os.close(hook_write_fd)


def test_hostile_lineage_is_reaped() -> None:
    """A setsid double-fork survives markers but not kernel-lineage cleanup."""
    with fixture() as case:
        control = case.control()
        outcome = signal_and_finish(case, control, "hostile-tree", signal.SIGTERM)
        root = build_root(control)
        features = (control / "features").read_text(encoding="utf-8")
        assert "dumpable=0\n" in features, features
        assert "environ=0\n" in features, features
        assert "comm=6c696e650a6e616d650a\n" in features, features
        assert outcome.status == 128 + signal.SIGTERM, outcome
        assert not root_entry_exists(root), root
        assert_identities_gone(control)


def test_non_sigchld_clone_is_reaped() -> None:
    """The all-child reap includes a direct clone child with exit_signal=0."""
    module = load_supervisor_module()
    supervisor = module.Supervisor([], None, None, None)
    pid, gate_write_fd = raw_clone_without_exit_signal()
    process = None
    gate_open = True
    try:
        process = supervisor._new_process(
            pid,
            f"test-parent={os.getpid()}",
            require_parent_pid=os.getpid(),
        )
        assert process is not None, "raw clone vanished before registration"
        os.write(gate_write_fd, b"x")
        os.close(gate_write_fd)
        gate_open = False
        readable, _writable, _exceptional = select.select(
            [process.pidfd],
            [],
            [],
            2.0,
        )
        assert readable, f"raw clone did not exit: pid={pid}"
        supervisor._reap_available()
        assert process.reaped, "non-SIGCHLD clone was excluded from waitpid"
        assert process.wait_status is not None
        assert os.WIFEXITED(process.wait_status)
        assert os.WEXITSTATUS(process.wait_status) == 0
    finally:
        if gate_open:
            os.write(gate_write_fd, b"x")
            os.close(gate_write_fd)
        if process is None or not process.reaped:
            os.waitpid(pid, WAIT_ALL_CHILDREN)
        if process is not None and not process.reaped:
            os.close(process.pidfd)


def test_direct_adoption_replaces_reaped_pid_entry() -> None:
    """A newly adopted direct child replaces stale bookkeeping for reused PID."""
    with tempfile.TemporaryDirectory(prefix="libtmux-reused-direct-") as raw:
        control = pathlib.Path(raw)
        child = subprocess.Popen(
            [sys.executable, "-c", "import signal; signal.pause()"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        start_time = proc_start_time(child.pid)
        assert start_time is not None
        append_identity_record(control, "reused-direct", child.pid, start_time)
        module = load_supervisor_module()
        latch = module.SignalLatch()
        owner = module.Supervisor([], None, latch, None)
        assert owner._supervisor_children() == {child.pid}
        stale = module.TrackedProcess(
            child.pid,
            -1,
            start_time - 1,
            "reaped-before-pid-reuse",
            reaped=True,
            wait_status=0,
        )
        owner.processes[child.pid] = stale
        try:
            assert owner._discover_supervisor_children()
            replacement = owner.processes[child.pid]
            assert replacement is not stale
            assert not replacement.reaped
            assert replacement.start_time == start_time
        finally:
            kill_recorded_identities(control)
            child.wait(timeout=2.0)
            owner.close_pidfds()
            latch.restore()


def test_recursive_freeze_replaces_reaped_pid_entry() -> None:
    """A nested child replaces stale reused-PID bookkeeping during freeze."""
    with tempfile.TemporaryDirectory(prefix="libtmux-reused-nested-") as raw:
        control = pathlib.Path(raw)
        parent = subprocess.Popen(
            [sys.executable, "-c", REUSED_PID_PARENT_SOURCE, os.fspath(control)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        wait_for_path(parent, control / "ready")
        child_pid = int((control / "child-pid").read_text(encoding="utf-8"))
        child_start = next(
            start_time
            for label, pid, start_time in identities(control)
            if label == "reused-nested" and pid == child_pid
        )
        module = load_supervisor_module()
        latch = module.SignalLatch()
        owner = module.Supervisor([], None, latch, None)
        tracked_parent = owner._new_process(
            parent.pid,
            f"test-parent={os.getpid()}",
            require_parent_pid=os.getpid(),
        )
        assert tracked_parent is not None
        stale = module.TrackedProcess(
            child_pid,
            -1,
            child_start - 1,
            "reaped-before-pid-reuse",
            reaped=True,
            wait_status=0,
        )
        owner.processes[child_pid] = stale
        try:
            owner._freeze_fixed_point(time.monotonic() + 2.0)
            replacement = owner.processes[child_pid]
            assert replacement is not stale
            assert not replacement.reaped
            assert replacement.start_time == child_start
        finally:
            kill_recorded_identities(control)
            parent.wait(timeout=2.0)
            owner.close_pidfds()
            latch.restore()


def test_term_handler_fork_escape_is_reaped() -> None:
    """A child forked by a TERM handler stays in the subreaper closure."""
    with fixture() as case:
        control = case.control()
        process = supervisor_process(case, control, "term-fork")
        wait_for_path(process, control / "ready")
        started = time.monotonic()
        os.kill(process.pid, signal.SIGTERM)
        wait_for_path(process, control / "escaped")
        outcome = finish(process, started)
        root = build_root(control)
        labels = {label for label, _pid, _start in identities(control)}
        assert "term-escape" in labels, labels
        assert outcome.status == 128 + signal.SIGTERM, outcome
        assert not root_entry_exists(root), root
        assert_identities_gone(control)


def test_kill_closure_discovers_adopted_frontier() -> None:
    """KILL repeatedly adopts and reaps a nested frontier after freeze failure."""
    injected_control_raw = os.environ.get(FRONTIER_TIMEOUT_CONTROL_ENV)
    injected_control = (
        pathlib.Path(injected_control_raw) if injected_control_raw is not None else None
    )
    with fixture() as case:
        control = case.control()
        process = supervisor_process(
            case,
            control,
            "kill-frontier",
            fault="freeze",
        )
        wait_for_path(process, control / "ready")
        root = build_root(control)
        started = time.monotonic()
        try:
            if injected_control is not None:
                (injected_control / "frontier-root").write_text(
                    os.fspath(root),
                    encoding="utf-8",
                )
                record_process_identity(
                    injected_control,
                    "frontier-supervisor",
                    process,
                )
                copy_identity_records(control, injected_control)
                pidfd = os.pidfd_open(process.pid)
                try:
                    signal.pidfd_send_signal(pidfd, signal.SIGSTOP)
                finally:
                    os.close(pidfd)
                raise subprocess.TimeoutExpired(process.args, WAIT_TIMEOUT)
            os.kill(process.pid, signal.SIGTERM)
            outcome = finish(process, started)
            labels = {label for label, _pid, _start in identities(control)}
            assert labels == {
                "kill-frontier-0",
                "kill-frontier-1",
                "kill-frontier-2",
                "kill-frontier-3",
            }, labels
            assert outcome.status == 128 + signal.SIGTERM, outcome
            assert not root_entry_exists(root), root
            assert_identities_gone(control)
        finally:
            if injected_control is None:
                kill_recorded_identities(control)
            if injected_control is not None and root_entry_exists(root):
                (injected_control / "root-retained-for-owner").touch()
                if os.environ.get(FRONTIER_MUTATION_HOLD_ENV) == "1":
                    (injected_control / "frontier-inner-finished").touch()
                    signal.raise_signal(signal.SIGSTOP)


def test_frontier_timeout_defers_root_cleanup_to_enclosing_owner() -> None:
    """A local timeout cannot delete before enclosing lineage closure."""
    with tempfile.TemporaryDirectory(prefix="libtmux-frontier-timeout-") as raw:
        control = pathlib.Path(raw)
        try:
            status = run_selected_cases(
                ["test_kill_closure_discovers_adopted_frontier"],
                case_timeout=3.0,
                case_cleanup_timeout=0.5,
                environment={
                    FRONTIER_TIMEOUT_CONTROL_ENV: os.fspath(control),
                },
            )
            root = pathlib.Path(
                (control / "frontier-root").read_text(encoding="utf-8").strip()
            )
            assert status == 1, status
            assert (control / "root-retained-for-owner").exists(), control
            assert not root_entry_exists(root), root
            assert_identities_gone(control)
        finally:
            kill_recorded_identities(control)


def test_frontier_residue_reaches_outer_containment() -> None:
    """Only the enclosing pidfd/subreaper owner removes frontier residue."""
    expected_labels = {
        "kill-frontier-0",
        "kill-frontier-1",
        "kill-frontier-2",
        "kill-frontier-3",
    }
    with tempfile.TemporaryDirectory(prefix="libtmux-frontier-owner-proof-") as raw:
        control = pathlib.Path(raw)
        live_before_containment: set[str] = set()

        def stop_outer_before_containment(process: subprocess.Popen[str]) -> None:
            nonlocal live_before_containment
            wait_for_path(process, control / "frontier-inner-finished")
            expected_start = require_process_start_time(process)
            pidfd = os.pidfd_open(process.pid)
            try:
                if proc_start_time(process.pid) != expected_start:
                    raise AssertionError("outer supervisor identity changed")
                signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                process.wait(timeout=TOPMOST_CLEANUP_TIMEOUT)
            finally:
                os.close(pidfd)
            live_before_containment = live_identity_labels(
                control,
                "kill-frontier-",
            )

        try:
            outcome = run_isolated_case(
                "test_kill_closure_discovers_adopted_frontier",
                timeout=3.0,
                cleanup_timeout=0.5,
                environment={
                    FRONTIER_TIMEOUT_CONTROL_ENV: os.fspath(control),
                    FRONTIER_MUTATION_HOLD_ENV: "1",
                },
                after_owner_validation=stop_outer_before_containment,
            )
            root = pathlib.Path(
                (control / "frontier-root").read_text(encoding="utf-8").strip()
            )
            assert live_before_containment == expected_labels, live_before_containment
            assert outcome.status == -signal.SIGKILL, outcome
            assert (control / "root-retained-for-owner").exists(), control
            assert not root_entry_exists(root), root
            assert_identities_gone(control)
        finally:
            kill_recorded_identities(control)


def run_preflight_failure(
    case: Fixture,
    control: pathlib.Path,
    fault: str,
) -> Outcome:
    """Collect a preflight fault, interrupting an incorrectly resumed worker."""
    process = supervisor_process(case, control, "preflight", fault=fault)
    deadline = time.monotonic() + 5.0
    while process.poll() is None and time.monotonic() < deadline:
        if (control / "continued").exists():
            os.kill(process.pid, signal.SIGTERM)
            break
        time.sleep(0.01)
    return finish(process, time.monotonic())


def test_supervisor_children_are_preflighted() -> None:
    """Unavailable supervisor child enumeration prevents worker continuation."""
    with fixture() as case:
        control = case.control()
        outcome = run_preflight_failure(
            case,
            control,
            "supervisor-children-unavailable",
        )
        root = build_root(control)
        assert outcome.status == 1, outcome
        assert not (control / "continued").exists(), outcome
        assert "injected supervisor children failure" in outcome.stderr
        assert root_entry_exists(root), root


def test_worker_children_are_preflighted() -> None:
    """Unavailable stopped-worker child enumeration prevents continuation."""
    with fixture() as case:
        control = case.control()
        outcome = run_preflight_failure(
            case,
            control,
            "worker-children-unavailable",
        )
        root = build_root(control)
        assert outcome.status == 1, outcome
        assert not (control / "continued").exists(), outcome
        assert "injected worker children failure" in outcome.stderr
        assert root_entry_exists(root), root


def test_unrelated_sentinel_is_untouched() -> None:
    """Cleanup never selects an unrelated same-uid process."""
    sentinel = subprocess.Popen([sys.executable, "-c", "import signal; signal.pause()"])
    try:
        with fixture() as case:
            control = case.control()
            outcome = signal_and_finish(case, control, "hostile-tree", signal.SIGTERM)
            assert outcome.status == 128 + signal.SIGTERM, outcome
            assert sentinel.poll() is None, f"unrelated pid exited: {sentinel.pid}"
            assert_identities_gone(control)
    finally:
        sentinel.terminate()
        try:
            sentinel.wait(timeout=2)
        except subprocess.TimeoutExpired:
            sentinel.kill()
            sentinel.wait(timeout=2)


def test_path_only_cleanup_refuses_a_matching_replacement() -> None:
    """A matching path cannot become emergency recursive-delete authority."""
    module = t.cast(CleanupModule, load_supervisor_module())
    original = create_owned_root(module, require_parent=True)
    original_name = original.name
    moved_name = f"{original_name}.moved"
    replacement: OwnedBuildRoot | None = None
    try:
        os.rename(
            original_name,
            moved_name,
            src_dir_fd=original.parent_fd,
            dst_dir_fd=original.parent_fd,
        )
        original.name = moved_name
        replacement = create_named_owned_root(
            module,
            original.parent_fd,
            original.parent_path,
            original_name,
        )
        touch_owned_root(replacement, "replacement-sentinel")

        assert "delete_test_root" not in globals()
        assert "capture_test_root" not in globals()
        assert owned_root_has_entry(replacement, "replacement-sentinel")
    finally:
        if replacement is None:
            cleanup_exact_owners(original)
        else:
            cleanup_exact_owners(replacement, original)


def test_named_root_mount_drift_is_refused_before_mutation() -> None:
    """The freshly opened named root must retain its creation mount ID."""
    module = t.cast(CleanupModule, load_supervisor_module())
    root = create_owned_root(module, require_parent=True)
    touch_owned_root(root, "mount-drift-sentinel")
    original_mount_id = module._mount_id

    def changed_named_mount_id(descriptor: int) -> int:
        opened = os.fstat(descriptor)
        if (
            descriptor != root.root_fd
            and opened.st_dev == root.device
            and opened.st_ino == root.inode
        ):
            return root.mount_id + 1
        return original_mount_id(descriptor)

    failure: Exception | None = None
    module._mount_id = changed_named_mount_id
    try:
        try:
            root.delete()
        except Exception as error:
            failure = error
        assert failure is not None, "named mount drift was not rejected"
        assert "mount" in str(failure), failure
        assert owned_root_has_entry(root, "mount-drift-sentinel")
    finally:
        module._mount_id = original_mount_id
        cleanup_exact_owners(root)


def test_regular_file_mount_drift_is_refused_before_mutation() -> None:
    """A file mount crossing is rejected before any sibling is removed."""
    module = t.cast(CleanupModule, load_supervisor_module())
    root = create_owned_root(module, require_parent=True)
    touch_owned_root(root, "a-sibling-sentinel")
    touch_owned_root(root, "z-file-mount")
    mounted = os.stat(
        "z-file-mount",
        dir_fd=root.root_fd,
        follow_symlinks=False,
    )
    original_mount_id = module._mount_id

    def different_file_mount_id(descriptor: int) -> int:
        opened = os.fstat(descriptor)
        if opened.st_dev == mounted.st_dev and opened.st_ino == mounted.st_ino:
            return root.mount_id + 1
        return original_mount_id(descriptor)

    failure: Exception | None = None
    module._mount_id = different_file_mount_id
    try:
        try:
            root.delete()
        except Exception as error:
            failure = error
        assert failure is not None, "regular-file mount drift was not rejected"
        assert "mount" in str(failure), failure
        assert owned_root_has_entry(root, "a-sibling-sentinel")
        assert owned_root_has_entry(root, "z-file-mount")
    finally:
        module._mount_id = original_mount_id
        cleanup_exact_owners(root)


def test_preclaim_exchange_is_refused_before_recursive_mutation() -> None:
    """A replacement exchanged before atomic claim is retained unchanged."""
    module = t.cast(CleanupModule, load_supervisor_module())
    original = t.cast(
        MutableBuildRoot,
        create_owned_root(module, require_parent=True),
    )
    replacement = create_owned_root(module, require_parent=True)
    original_name = original.name
    replacement_name = replacement.name
    touch_owned_root(original, "original-sentinel")
    touch_owned_root(replacement, "replacement-sentinel")
    verify_root_name = original._verify_root_name
    exchanged = False

    def exchange_after_validation() -> None:
        nonlocal exchanged
        verify_root_name()
        if not exchanged:
            rename_exchange(
                original.parent_fd,
                original_name,
                replacement.parent_fd,
                replacement_name,
            )
            exchanged = True

    original._verify_root_name = exchange_after_validation  # type: ignore[method-assign]
    try:
        failure: Exception | None = None
        try:
            original.delete()
        except Exception as error:
            failure = error
        assert failure is not None, "pre-claim exchange was accepted"
        assert owned_root_has_entry(original, "original-sentinel")
        assert owned_root_has_entry(replacement, "replacement-sentinel")
    finally:
        del original._verify_root_name
        if exchanged:
            original.name = replacement_name
            replacement.name = original_name
        cleanup_exact_owners(original, replacement)


def test_root_replacement_is_refused() -> None:
    """A replacement at the build-root name is retained and never traversed."""
    with fixture() as case:
        control = case.control()
        outcome = run_to_completion(case, control, "replace-root")
        root = build_root(control)
        moved = pathlib.Path(f"{root}.moved")
        assert outcome.status == 1, outcome
        assert "refusing to delete replaced build root" in outcome.stderr
        assert "retained build root" in outcome.stderr
        assert root.is_symlink(), root
        assert (root / "replacement-sentinel").exists(), root
        assert (moved / "original-sentinel").exists(), moved


def test_same_device_mount_boundary_is_refused() -> None:
    """A different mount ID is retained even when st_dev is unchanged."""
    module = load_supervisor_module()
    mount_namespace = t.cast(MountIdNamespace, module)
    root = create_owned_root(
        t.cast(CleanupModule, module),
        require_parent=True,
    )
    pause_after_case_root(PAUSE_SAME_DEVICE_ROOT, pathlib.Path(root.path))
    original_mount_id = t.cast(
        t.Callable[[int], int] | None,
        getattr(module, "_mount_id", None),
    )
    nested_fd: int | None = None
    try:
        os.mkdir("same-device-mount", dir_fd=root.root_fd)
        nested_fd = os.open(
            "same-device-mount",
            os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
            dir_fd=root.root_fd,
        )
        sentinel_fd = os.open(
            "sentinel",
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC,
            0o600,
            dir_fd=nested_fd,
        )
        os.close(sentinel_fd)
        nested_stat = os.fstat(nested_fd)
        assert nested_stat.st_dev == root.device
        assert original_mount_id is not None, "mount identity query is unavailable"
        assert hasattr(root, "mount_id"), "build root mount identity is unavailable"
        root_mount_id = root.mount_id

        def different_child_mount_id(descriptor: int) -> int:
            opened = os.fstat(descriptor)
            if (
                opened.st_dev == nested_stat.st_dev
                and opened.st_ino == nested_stat.st_ino
            ):
                return root_mount_id + 1
            return original_mount_id(descriptor)

        mount_namespace._mount_id = different_child_mount_id
        try:
            root.delete()
        except module.SupervisorFailure as error:
            assert "refusing to cross a mount" in str(error)
        else:
            raise AssertionError("same-device mount boundary was traversed")
        os.stat("sentinel", dir_fd=nested_fd, follow_symlinks=False)
    finally:
        if original_mount_id is not None:
            mount_namespace._mount_id = original_mount_id
        if nested_fd is not None:
            os.close(nested_fd)
        cleanup_exact_owners(root)


def test_injected_failure_retains_root() -> None:
    """A lifecycle fault overrides success and retains the exact build root."""
    with fixture() as case:
        control = case.control()
        outcome = run_to_completion(
            case,
            control,
            "retain-root",
            fault="proc-children",
        )
        root = build_root(control)
        assert outcome.status == 1, outcome
        assert "injected proc children failure" in outcome.stderr
        assert "compatibility process closure remains unproven" in outcome.stderr
        assert "retained build root" in outcome.stderr
        assert (root / "retained-sentinel").exists(), root


def test_dangling_symlink_root_residue_is_rejected() -> None:
    """A dangling supervised root name fails before enclosing cleanup."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-dangling-root-") as raw:
        control = pathlib.Path(raw)
        outcome = run_isolated_case(
            RUNNER_DANGLING_ROOT_PROBE,
            timeout=2.0,
            cleanup_timeout=0.5,
            environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
        )
        root = pathlib.Path(
            (control / "dangling-root").read_text(encoding="utf-8").strip()
        )
        assert outcome.status == 1, outcome
        assert "supervised root entry remains" in outcome.stderr, outcome
        assert not root_entry_exists(root), root


def test_top_level_runner_term_closes_the_active_case() -> None:
    """The top-level runner contains its active case before signal exit."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-signal-") as raw:
        control = pathlib.Path(raw)
        process = spawn_nested_top_runner(RUNNER_TIMEOUT_PROBE, control)
        pidfd = os.pidfd_open(process.pid)
        try:
            record_process_identity(control, "top-level-runner", process)
            wait_for_path(process, control / "ready")
            root = pathlib.Path(
                (control / "outer-root").read_text(encoding="utf-8").strip()
            )
            signal.pidfd_send_signal(pidfd, signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=WAIT_TIMEOUT)
            assert process.returncode == 128 + signal.SIGTERM, (
                process.returncode,
                stdout,
                stderr,
            )
            assert not root_entry_exists(root), root
            assert_identities_gone(control)
        finally:
            if process.poll() is None:
                with contextlib.suppress(ProcessLookupError):
                    signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                process.wait(timeout=TOPMOST_CLEANUP_TIMEOUT)
            os.close(pidfd)
            kill_recorded_identities(control)


def test_top_runner_pending_signal_at_blocked_allocation_cutover() -> None:
    """A signal pending under the runner allocation block controls status."""
    with fixture() as case:
        control = case.control()
        hook_read_fd, hook_write_fd = os.pipe2(os.O_CLOEXEC)
        hook_target_fd = hook_write_fd
        process: subprocess.Popen[str] | None = None
        pidfd: int | None = None
        try:
            process = spawn_nested_top_runner(
                RUNNER_CLEAN_EXIT_PROBE,
                control,
                {RUNNER_CUTOVER_STOP_FD_ENV: str(hook_write_fd)},
                (hook_write_fd,),
            )
            os.close(hook_write_fd)
            hook_write_fd = -1
            record_process_identity(control, "pending-top-runner", process)
            expected_start = require_process_start_time(process)
            pidfd = os.pidfd_open(process.pid)
            wait_for_hook_marker(process, hook_read_fd)
            wait_for_exact_process_stopped(process, expected_start)
            assert_hook_capability_consumed(process.pid, hook_target_fd)
            assert_signal_blocked(process.pid, signal.SIGHUP)
            signal.pidfd_send_signal(pidfd, signal.SIGHUP)
            wait_for_signal_pending(process, signal.SIGHUP)
            wait_for_exact_process_stopped(process, expected_start)
            signal.pidfd_send_signal(pidfd, signal.SIGCONT)
            stdout, stderr = process.communicate(timeout=WAIT_TIMEOUT)
            assert process.returncode == 128 + signal.SIGHUP, (
                process.returncode,
                stdout,
                stderr,
            )
            assert not (control / "outer-root").exists(), control
            assert_identities_gone(control)
        finally:
            if process is not None and process.poll() is None:
                if pidfd is not None:
                    with contextlib.suppress(ProcessLookupError):
                        signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                else:
                    process.kill()
                process.communicate(timeout=TOPMOST_CLEANUP_TIMEOUT)
            if pidfd is not None:
                os.close(pidfd)
            os.close(hook_read_fd)
            if hook_write_fd >= 0:
                os.close(hook_write_fd)
            kill_recorded_identities(control)


def test_failed_case_has_outer_containment() -> None:
    """A failed case cannot leave its setsid descendant or build root."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-failure-") as raw:
        control = pathlib.Path(raw)
        status = run_selected_cases(
            [RUNNER_FAILURE_PROBE],
            case_timeout=2.0,
            environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
        )
        root = pathlib.Path(
            (control / "outer-root").read_text(encoding="utf-8").strip()
        )
        assert status == 1, status
        assert not root_entry_exists(root), root
        assert_identities_gone(control)


def test_timed_out_case_has_outer_containment() -> None:
    """A timed-out case is bounded and cannot leave a setsid descendant."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-timeout-") as raw:
        control = pathlib.Path(raw)
        case_timeout = 0.5
        started = time.monotonic()
        status = run_selected_cases(
            [RUNNER_TIMEOUT_PROBE],
            case_timeout=case_timeout,
            environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
        )
        elapsed = time.monotonic() - started
        root = pathlib.Path(
            (control / "outer-root").read_text(encoding="utf-8").strip()
        )
        assert status == 1, status
        assert elapsed < 6.5, elapsed
        assert not root_entry_exists(root), root
        assert_identities_gone(control)


def test_failed_case_cleans_nested_supervisor_root() -> None:
    """Assertion containment removes nested supervisor roots and identities."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-inner-failure-") as raw:
        control = pathlib.Path(raw)
        try:
            status = run_selected_cases(
                [RUNNER_INNER_FAILURE_PROBE],
                case_timeout=2.0,
                environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
            )
            inner_root = pathlib.Path(
                (control / "inner-root").read_text(encoding="utf-8").strip()
            )
            outer_root = pathlib.Path(
                (control / "outer-root").read_text(encoding="utf-8").strip()
            )
            assert status == 1, status
            assert not root_entry_exists(outer_root), outer_root
            assert not root_entry_exists(inner_root), inner_root
            assert_identities_gone(control)
        finally:
            kill_recorded_identities(control)


def test_timed_out_case_cleans_nested_supervisor_root() -> None:
    """Timeout containment removes nested supervisor roots and identities."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-inner-timeout-") as raw:
        control = pathlib.Path(raw)
        started = time.monotonic()
        try:
            status = run_selected_cases(
                [RUNNER_INNER_TIMEOUT_PROBE],
                case_timeout=0.5,
                environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
            )
            elapsed = time.monotonic() - started
            inner_root = pathlib.Path(
                (control / "inner-root").read_text(encoding="utf-8").strip()
            )
            outer_root = pathlib.Path(
                (control / "outer-root").read_text(encoding="utf-8").strip()
            )
            assert status == 1, status
            assert elapsed < 6.5, elapsed
            assert not root_entry_exists(outer_root), outer_root
            assert not root_entry_exists(inner_root), inner_root
            assert_identities_gone(control)
        finally:
            kill_recorded_identities(control)


def test_topmost_failure_has_independent_containment() -> None:
    """A stopped outer supervisor cannot escape the top-level pidfd owner."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-topmost-") as raw:
        control = pathlib.Path(raw)
        started = time.monotonic()
        try:
            status = run_selected_cases(
                [RUNNER_TOPMOST_FAILURE_PROBE],
                case_timeout=3.0,
                case_cleanup_timeout=0.25,
                environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
            )
            elapsed = time.monotonic() - started
            inner_root = pathlib.Path(
                (control / "inner-root").read_text(encoding="utf-8").strip()
            )
            outer_root = pathlib.Path(
                (control / "outer-root").read_text(encoding="utf-8").strip()
            )
            assert status == 1, status
            assert elapsed < 7.0, elapsed
            assert not root_entry_exists(outer_root), outer_root
            assert not root_entry_exists(inner_root), inner_root
            assert_identities_gone(control)
        finally:
            kill_recorded_identities(control)


def test_untrusted_case_cannot_report_an_unrelated_root() -> None:
    """A worker cannot submit an unrelated matching directory for deletion."""
    module = t.cast(CleanupModule, load_supervisor_module())
    forged_owner = create_owned_root(module, require_parent=True)
    forged_root = pathlib.Path(forged_owner.parent_path) / forged_owner.name
    pause_after_case_root(PAUSE_UNTRUSTED_ROOT, forged_root)
    sentinel = forged_root / "untrusted-report-sentinel"
    sentinel.touch()
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-forged-root-") as raw:
        control = pathlib.Path(raw)
        try:
            outcome = run_isolated_case(
                RUNNER_FORGED_ROOT_PROBE,
                timeout=2.0,
                cleanup_timeout=0.5,
                environment={
                    "TFC_RUNNER_CONTROL": os.fspath(control),
                    FORGED_ROOT_ENV: os.fspath(forged_root),
                },
            )
            assert outcome.status == 0, outcome
            assert (control / "report-capability-absent").exists(), control
            assert not (control / "forged-report-sent").exists(), control
            assert sentinel.exists(), forged_root
        finally:
            failure = cleanup_owned_root(forged_owner)
            if failure is not None:
                raise AssertionError(failure)


def test_post_exit_direct_child_forces_top_level_closure() -> None:
    """A direct child left after outer exit is killed before root deletion."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-post-exit-") as raw:
        control = pathlib.Path(raw)
        holders: list[subprocess.Popen[str]] = []
        outer_root: pathlib.Path | None = None

        def retain_direct_child(_process: subprocess.Popen[str]) -> None:
            holders.append(spawn_direct_child_holder(control))

        try:
            outcome = run_isolated_case(
                RUNNER_CLEAN_EXIT_PROBE,
                timeout=2.0,
                cleanup_timeout=0.5,
                environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
                after_owner_validation=retain_direct_child,
            )
            outer_root = pathlib.Path(
                (control / "outer-root").read_text(encoding="utf-8").strip()
            )
            assert outcome.status == 0, outcome
            assert_identities_gone(control)
            assert not root_entry_exists(outer_root), outer_root
        finally:
            kill_recorded_identities(control)
            for holder in holders:
                holder.wait(timeout=TOPMOST_CLEANUP_TIMEOUT)


def test_owner_validation_failure_forces_top_level_closure() -> None:
    """An owner-validation exception closes the exact live outer lineage."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-owner-error-") as raw:
        control = pathlib.Path(raw)
        holders: list[subprocess.Popen[str]] = []
        outer_processes: list[subprocess.Popen[str]] = []
        outer_root: pathlib.Path | None = None

        def add_unexpected_direct_child(
            process: subprocess.Popen[str],
        ) -> None:
            nonlocal outer_root
            wait_for_path(process, control / "ready")
            outer_root = pathlib.Path(
                (control / "outer-root").read_text(encoding="utf-8").strip()
            )
            outer_processes.append(process)
            record_process_identity(control, "outer-supervisor", process)
            holders.append(spawn_direct_child_holder(control))

        failure: AssertionError | None = None
        try:
            try:
                run_isolated_case(
                    RUNNER_TIMEOUT_PROBE,
                    timeout=2.0,
                    cleanup_timeout=0.5,
                    environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
                    before_owner_validation=add_unexpected_direct_child,
                )
            except AssertionError as error:
                failure = error
            assert failure is not None, "owner validation unexpectedly succeeded"
            assert "sole direct child" in str(failure), failure
            assert outer_root is not None
            assert not root_entry_exists(outer_root), outer_root
            assert_identities_gone(control)
        finally:
            kill_recorded_identities(control)
            for process in [*holders, *outer_processes]:
                process.wait(timeout=TOPMOST_CLEANUP_TIMEOUT)


def test_hard_kill_after_child_root_creation_cleans_container() -> None:
    """A parent-owned container covers a child killed after root creation."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-root-create-") as raw:
        control = pathlib.Path(raw)
        outcome: Outcome | None = None
        failure: AssertionError | None = None
        try:
            outcome = run_isolated_case(
                RUNNER_CLEAN_EXIT_PROBE,
                timeout=0.5,
                cleanup_timeout=0.25,
                environment={
                    AFTER_ROOT_CREATE_CONTROL_ENV: os.fspath(control),
                },
                outer_fault="after-root-create",
            )
        except AssertionError as error:
            failure = error
        root = pathlib.Path(
            (control / "after-root-create-root").read_text(encoding="utf-8").strip()
        )
        assert failure is None, failure
        assert outcome is not None and outcome.timed_out, outcome
        assert not root_entry_exists(root), root


def assert_forced_case_timeout_cleans_root(test_name: str, pause_label: str) -> None:
    """Force-kill one allocated case and require its enclosing owner to clean."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-case-root-") as raw:
        control = pathlib.Path(raw)
        status = run_selected_cases(
            [test_name],
            case_timeout=2.0,
            case_cleanup_timeout=0.25,
            environment={
                PAUSE_CASE_ROOT_ENV: pause_label,
                PAUSE_CASE_ROOT_CONTROL_ENV: os.fspath(control),
            },
        )
        root = pathlib.Path(
            (control / "allocated-root").read_text(encoding="utf-8").strip()
        )
        assert status == 1, status
        assert not root_entry_exists(root), root


def test_same_device_mount_case_timeout_cleans_allocated_root() -> None:
    """Forced timeout cannot orphan the mount-boundary case root."""
    assert_forced_case_timeout_cleans_root(
        "test_same_device_mount_boundary_is_refused",
        PAUSE_SAME_DEVICE_ROOT,
    )


def test_untrusted_report_case_timeout_cleans_allocated_root() -> None:
    """Forced timeout cannot orphan the forged-report case root."""
    assert_forced_case_timeout_cleans_root(
        "test_untrusted_case_cannot_report_an_unrelated_root",
        PAUSE_UNTRUSTED_ROOT,
    )


def test_same_name_root_replacement_is_not_deleted() -> None:
    """Late cleanup refuses a same-name directory with a new identity."""
    with tempfile.TemporaryDirectory(prefix="libtmux-runner-root-identity-") as raw:
        control = pathlib.Path(raw)
        failure: AssertionError | None = None
        try:
            run_isolated_case(
                RUNNER_ROOT_REPLACEMENT_PROBE,
                timeout=2.0,
                cleanup_timeout=0.5,
                environment={"TFC_RUNNER_CONTROL": os.fspath(control)},
            )
        except AssertionError as error:
            failure = error
        root = pathlib.Path(
            (control / "inner-root").read_text(encoding="utf-8").strip()
        )
        moved = pathlib.Path(
            (control / "inner-root-moved").read_text(encoding="utf-8").strip()
        )
        assert failure is not None, "same-name replacement was accepted"
        assert "creation identity changed" in str(failure), failure
        assert (root / "replacement-sentinel").exists(), root
        assert (moved / "original-sentinel").exists(), moved


def test_parallel_runs_are_isolated() -> None:
    """Concurrent supervisors use distinct roots and close independently."""
    with fixture() as case:
        controls = [case.control(f"control-{index}") for index in range(4)]
        started = time.monotonic()
        processes = [
            supervisor_process(case, control, "parallel") for control in controls
        ]
        for process, control in zip(processes, controls, strict=True):
            wait_for_path(process, control / "ready")
        roots = [build_root(control) for control in controls]
        assert len(set(roots)) == len(roots), roots
        for control in controls:
            (control / "release").touch()
        outcomes = [finish(process, started) for process in processes]
        assert [outcome.status for outcome in outcomes] == [0, 0, 0, 0], outcomes
        assert all(not root_entry_exists(root) for root in roots), roots


TESTS: dict[str, t.Callable[[], None]] = {
    function.__name__: function
    for function in (
        test_status_37_and_streams,
        test_wrapper_skips_python_without_pidfd,
        test_hup_int_term_statuses,
        test_second_signal_accelerates_without_replacing_status,
        test_late_first_signal_replaces_the_worker_status_at_cutover,
        test_hostile_lineage_is_reaped,
        test_non_sigchld_clone_is_reaped,
        test_direct_adoption_replaces_reaped_pid_entry,
        test_recursive_freeze_replaces_reaped_pid_entry,
        test_term_handler_fork_escape_is_reaped,
        test_kill_closure_discovers_adopted_frontier,
        test_frontier_timeout_defers_root_cleanup_to_enclosing_owner,
        test_frontier_residue_reaches_outer_containment,
        test_supervisor_children_are_preflighted,
        test_worker_children_are_preflighted,
        test_unrelated_sentinel_is_untouched,
        test_path_only_cleanup_refuses_a_matching_replacement,
        test_named_root_mount_drift_is_refused_before_mutation,
        test_regular_file_mount_drift_is_refused_before_mutation,
        test_preclaim_exchange_is_refused_before_recursive_mutation,
        test_root_replacement_is_refused,
        test_same_device_mount_boundary_is_refused,
        test_injected_failure_retains_root,
        test_dangling_symlink_root_residue_is_rejected,
        test_top_level_runner_term_closes_the_active_case,
        test_top_runner_pending_signal_at_blocked_allocation_cutover,
        test_failed_case_has_outer_containment,
        test_timed_out_case_has_outer_containment,
        test_failed_case_cleans_nested_supervisor_root,
        test_timed_out_case_cleans_nested_supervisor_root,
        test_topmost_failure_has_independent_containment,
        test_untrusted_case_cannot_report_an_unrelated_root,
        test_post_exit_direct_child_forces_top_level_closure,
        test_owner_validation_failure_forces_top_level_closure,
        test_hard_kill_after_child_root_creation_cleans_container,
        test_same_device_mount_case_timeout_cleans_allocated_root,
        test_untrusted_report_case_timeout_cleans_allocated_root,
        test_same_name_root_replacement_is_not_deleted,
        test_parallel_runs_are_isolated,
    )
}

RUNNER_PROBES: dict[str, t.Callable[[pathlib.Path], None]] = {
    RUNNER_FAILURE_PROBE: runner_failure_probe,
    RUNNER_TIMEOUT_PROBE: runner_timeout_probe,
    RUNNER_INNER_FAILURE_PROBE: runner_inner_failure_probe,
    RUNNER_INNER_TIMEOUT_PROBE: runner_inner_timeout_probe,
    RUNNER_TOPMOST_FAILURE_PROBE: runner_topmost_failure_probe,
    RUNNER_ROOT_REPLACEMENT_PROBE: runner_root_replacement_probe,
    RUNNER_FORGED_ROOT_PROBE: runner_forged_root_probe,
    RUNNER_CLEAN_EXIT_PROBE: runner_clean_exit_probe,
    RUNNER_DANGLING_ROOT_PROBE: runner_dangling_root_probe,
}


def runner_child_pids() -> set[int]:
    """Return direct children from every task of the single-threaded runner."""
    children: set[int] = set()
    for task in pathlib.Path("/proc/self/task").iterdir():
        if not task.name.isdigit():
            continue
        for raw_pid in (task / "children").read_bytes().split():
            children.add(int(raw_pid))
    return children


def enable_top_level_subreaper() -> RunnerModule:
    """Enable exact adoption in a single-threaded child-free case runner."""
    task_ids = [
        entry
        for entry in pathlib.Path("/proc/self/task").iterdir()
        if entry.name.isdigit()
    ]
    if len(task_ids) != 1:
        raise AssertionError("top-level containment requires a single thread")
    if runner_child_pids():
        raise AssertionError("top-level containment requires no existing children")
    module = t.cast(RunnerModule, load_supervisor_module())
    module._enable_and_verify_subreaper()
    if runner_child_pids():
        raise AssertionError("subreaper preflight found an unexpected child")
    return module


def _take_runner_cutover_stop_fd() -> int | None:
    """Consume the runner's private pipe capability before case allocation."""
    raw_fd = os.environ.pop(RUNNER_CUTOVER_STOP_FD_ENV, None)
    if raw_fd is None:
        return None
    try:
        descriptor = int(raw_fd)
    except ValueError:
        return None
    if descriptor <= 2:
        return None
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISFIFO(opened.st_mode):
            os.close(descriptor)
            return None
        os.set_inheritable(descriptor, False)
        os.set_blocking(descriptor, False)
    except OSError:
        with contextlib.suppress(OSError):
            os.close(descriptor)
        return None
    return descriptor


def _test_stop_runner_after_mask(descriptor: int | None) -> None:
    """Publish, consume, and stop only for an explicit runner test hook."""
    if descriptor is None:
        return
    published = False
    try:
        published = os.write(descriptor, b"1") == 1
    except OSError:
        pass
    finally:
        os.close(descriptor)
    if published:
        signal.raise_signal(signal.SIGSTOP)


class RunnerSignalGuard:
    """Keep watched signals contained from case allocation through cleanup."""

    def __init__(
        self,
        module: RunnerModule,
        latch: RunnerSignalLatch,
        inherited_mask: set[int | signal.Signals],
    ) -> None:
        self.module = module
        self.latch = latch
        self.inherited_mask = inherited_mask
        self.finalized = False

    @classmethod
    def create(cls) -> RunnerSignalGuard:
        """Block signals before allocating any per-case ownership object."""
        inherited_mask = signal.pthread_sigmask(signal.SIG_BLOCK, WATCHED_SIGNALS)
        _test_stop_runner_after_mask(_take_runner_cutover_stop_fd())
        latch: RunnerSignalLatch | None = None
        try:
            module = enable_top_level_subreaper()
            latch = module.SignalLatch()
            latch.install()
            return cls(module, latch, inherited_mask)
        except Exception:
            if latch is not None:
                latch.restore()
            signal.pthread_sigmask(signal.SIG_SETMASK, inherited_mask)
            raise

    def first_signal(self) -> int | None:
        """Return the latched or blocked-pending first watched signal."""
        if self.latch.first is not None:
            return self.latch.first
        pending = signal.sigpending()
        for signal_number in WATCHED_SIGNALS:
            if signal_number in pending:
                return int(signal_number)
        return None

    def unblock_after_ownership(self) -> None:
        """Allow handlers to run only after root and pidfd ownership exist."""
        signal.pthread_sigmask(signal.SIG_SETMASK, self.inherited_mask)

    def block_for_cleanup(self) -> None:
        """Prevent default termination from interrupting exact cleanup."""
        signal.pthread_sigmask(signal.SIG_BLOCK, WATCHED_SIGNALS)

    def inherited_watched_mask(self) -> int:
        """Encode the caller's watched mask for the blocked child launch."""
        return sum(
            1 << int(signal_number)
            for signal_number in WATCHED_SIGNALS
            if signal_number in self.inherited_mask
        )

    def finalize(self, *, hold_mask: bool) -> int | None:
        """Select final signal status and restore handlers under the block."""
        if self.finalized:
            raise AssertionError("runner signal guard finalized twice")
        self.block_for_cleanup()
        final_signal = self.first_signal()
        self.latch.restore()
        self.finalized = True
        if not hold_mask and final_signal is None:
            signal.pthread_sigmask(signal.SIG_SETMASK, self.inherited_mask)
        return final_signal


def kill_top_level_children(module: RunnerModule) -> None:
    """Pidfd-kill and reap every child adopted by the top-level subreaper."""
    latch = module.SignalLatch()
    lineage = module.Supervisor([], None, latch, None)
    try:
        if not lineage._kill_closure(time.monotonic() + TOPMOST_CLEANUP_TIMEOUT):
            raise AssertionError("top-level adopted lineage did not close")
    finally:
        lineage.close_pidfds()
        latch.restore()


class TopLevelProcessOwner:
    """Hold the outer pidfd and reap its adopted lineage after hard failure."""

    def __init__(
        self,
        module: RunnerModule,
        process: subprocess.Popen[str],
    ) -> None:
        self.module = module
        self.process = process
        self.pidfd = os.pidfd_open(process.pid)
        self.start_time = proc_start_time(process.pid)

    def validate(self) -> None:
        """Validate the held outer identity and sole-child topology."""
        if self.start_time is None:
            raise AssertionError("outer supervisor vanished before pidfd ownership")
        if proc_start_time(self.process.pid) != self.start_time:
            raise AssertionError("outer supervisor identity changed during ownership")
        if runner_child_pids() != {self.process.pid}:
            raise AssertionError("outer supervisor is not the sole direct child")

    def send(self, signal_number: int) -> None:
        """Signal the exact outer supervisor identity through its held pidfd."""
        try:
            signal.pidfd_send_signal(self.pidfd, signal_number)
        except ProcessLookupError:
            return

    def force_closure(self) -> None:
        """Kill the outer identity, then kill and reap every adopted descendant."""
        if self.process.poll() is None:
            self.send(signal.SIGKILL)
        with contextlib.suppress(subprocess.TimeoutExpired):
            self.process.wait(timeout=TOPMOST_CLEANUP_TIMEOUT)
        kill_top_level_children(self.module)
        if self.process.poll() is None:
            raise AssertionError("outer supervisor resisted pidfd SIGKILL")

    def close(self) -> None:
        """Close the exact outer supervisor pidfd."""
        os.close(self.pidfd)


def communicate_with_containment(
    process: subprocess.Popen[str],
    owner: TopLevelProcessOwner,
    guard: RunnerSignalGuard,
    timeout: float,
    cleanup_timeout: float,
) -> tuple[str, str, bool]:
    """Collect streams while forwarding timeout or the first watched signal."""
    deadline = time.monotonic() + timeout
    cleanup_deadline: float | None = None
    timed_out = False
    while True:
        now = time.monotonic()
        first_signal = guard.first_signal()
        if first_signal is not None and cleanup_deadline is None:
            owner.send(first_signal)
            cleanup_deadline = now + cleanup_timeout
        elif process.poll() is not None and cleanup_deadline is None:
            cleanup_deadline = now + cleanup_timeout
        elif now >= deadline and cleanup_deadline is None:
            timed_out = True
            owner.send(signal.SIGTERM)
            cleanup_deadline = now + cleanup_timeout

        active_deadline = deadline if cleanup_deadline is None else cleanup_deadline
        if now >= active_deadline:
            owner.force_closure()
            stdout, stderr = process.communicate(timeout=TOPMOST_CLEANUP_TIMEOUT)
            return stdout, stderr, timed_out
        try:
            stdout, stderr = process.communicate(
                timeout=max(0.001, min(0.05, active_deadline - now))
            )
        except subprocess.TimeoutExpired:
            continue
        return stdout, stderr, timed_out


def isolated_case_process(
    name: str,
    environment: dict[str, str] | None,
    root_parent_fd: int,
    outer_fault: str | None,
    inherited_watched_mask: int,
) -> subprocess.Popen[str]:
    """Start one case beneath an independent outer supervisor."""
    python = os.path.realpath(sys.executable)
    test_script = os.fspath(pathlib.Path(__file__).resolve())
    child_environment = os.environ.copy()
    if environment is not None:
        child_environment.update(environment)
    child_environment.pop(CASE_ROOT_PARENT_FD_ENV, None)
    child_parent_fd = os.dup(root_parent_fd)
    command = [
        python,
        os.fspath(SUPERVISOR),
        f"--root-parent-fd={child_parent_fd}",
        "--pass-root-fd-to-worker",
        f"--inherited-watched-mask={inherited_watched_mask}",
    ]
    if outer_fault is not None:
        command.append(f"--test-fault={outer_fault}")
    command.extend(["--", python, test_script, INTERNAL_CASE_MODE, name])
    try:
        return subprocess.Popen(
            command,
            env=child_environment,
            pass_fds=(child_parent_fd,),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    finally:
        os.close(child_parent_fd)


def run_isolated_case(
    name: str,
    *,
    timeout: float,
    cleanup_timeout: float,
    environment: dict[str, str] | None,
    before_owner_validation: t.Callable[[subprocess.Popen[str]], None] | None = None,
    after_owner_validation: t.Callable[[subprocess.Popen[str]], None] | None = None,
    outer_fault: str | None = None,
    signal_guard: RunnerSignalGuard | None = None,
) -> Outcome:
    """Run one case with a bounded outer-supervisor cleanup interval."""
    started = time.monotonic()
    owns_signal_guard = signal_guard is None
    guard = RunnerSignalGuard.create() if signal_guard is None else signal_guard
    case_container: OwnedBuildRoot | None = None
    process: subprocess.Popen[str] | None = None
    process_owner: TopLevelProcessOwner | None = None
    outcome: Outcome | None = None
    final_signal: int | None = None
    try:
        try:
            case_container = create_owned_root(
                guard.module,
                require_parent=False,
            )
            process = isolated_case_process(
                name,
                environment,
                case_container.root_fd,
                outer_fault,
                guard.inherited_watched_mask(),
            )
            process_owner = TopLevelProcessOwner(guard.module, process)
            guard.unblock_after_ownership()
            if guard.first_signal() is None:
                if before_owner_validation is not None:
                    before_owner_validation(process)
                process_owner.validate()
                if after_owner_validation is not None:
                    after_owner_validation(process)
            stdout, stderr, timed_out = communicate_with_containment(
                process,
                process_owner,
                guard,
                timeout,
                cleanup_timeout,
            )
            outcome = Outcome(
                process.returncode,
                stdout,
                stderr,
                time.monotonic() - started,
                timed_out,
            )
        finally:
            guard.block_for_cleanup()
            closure_failure: Exception | None = None
            if process is not None:
                try:
                    if process_owner is None:
                        kill_top_level_children(guard.module)
                    else:
                        process_owner.force_closure()
                except Exception as error:
                    closure_failure = error
            if process_owner is not None:
                process_owner.close()
            if closure_failure is not None:
                if case_container is not None:
                    case_container.close()
                raise AssertionError(
                    "top-level process closure failed"
                ) from closure_failure
            if case_container is not None:
                cleanup_failure = cleanup_owned_root(case_container)
                if cleanup_failure is not None:
                    raise AssertionError(
                        "retained descriptor-owned root: " + cleanup_failure
                    )
    finally:
        if owns_signal_guard:
            final_signal = guard.finalize(hold_mask=False)
    if outcome is None:
        raise AssertionError("isolated case completed without an outcome")
    if final_signal is not None:
        return dataclasses.replace(outcome, status=128 + final_signal)
    return outcome


def run_internal_case(arguments: list[str]) -> int:
    """Self-stop, then execute one case as an outer-supervisor worker."""
    if len(arguments) != 2:
        print("invalid internal supervisor test invocation", file=sys.stderr)
        return 2
    name, raw_build_root = arguments
    public_case = TESTS.get(name)
    probe_case = RUNNER_PROBES.get(name)
    if public_case is None and probe_case is None:
        print(f"unknown internal supervisor test: {name}", file=sys.stderr)
        return 2

    raw_case_root_fd = os.environ.pop(CASE_ROOT_PARENT_FD_ENV, None)
    if raw_case_root_fd is None:
        print("internal case lacks its creation-time root descriptor", file=sys.stderr)
        return 2
    try:
        case_root_fd = int(raw_case_root_fd)
    except ValueError:
        print("invalid internal case root descriptor", file=sys.stderr)
        return 2
    if case_root_fd <= 2:
        print("invalid internal case root descriptor", file=sys.stderr)
        return 2
    os.set_inheritable(case_root_fd, False)
    module = t.cast(CleanupModule, load_supervisor_module())
    try:
        named_fd = os.open(
            raw_build_root,
            os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
        )
        try:
            held = os.fstat(case_root_fd)
            named = os.fstat(named_fd)
            held_mount_id = module._mount_id(case_root_fd)
            named_mount_id = module._mount_id(named_fd)
        finally:
            os.close(named_fd)
    except Exception as error:
        os.close(case_root_fd)
        print(f"internal case root verification failed: {error}", file=sys.stderr)
        return 2
    if (
        held.st_dev != named.st_dev
        or held.st_ino != named.st_ino
        or not stat.S_ISDIR(named.st_mode)
        or held_mount_id != named_mount_id
    ):
        os.close(case_root_fd)
        print("internal case root identity changed", file=sys.stderr)
        return 2
    os.environ[CASE_ROOT_PARENT_FD_ENV] = str(case_root_fd)
    try:
        os.kill(os.getpid(), signal.SIGSTOP)
        try:
            if public_case is not None:
                public_case()
            else:
                assert probe_case is not None
                probe_case(pathlib.Path(raw_build_root))
        except Exception:
            traceback.print_exc()
            return 1
        return 0
    finally:
        os.environ.pop(CASE_ROOT_PARENT_FD_ENV, None)
        os.close(case_root_fd)


def run_selected_cases(
    selected: list[str],
    *,
    case_timeout: float = CASE_TIMEOUT,
    case_cleanup_timeout: float = CASE_CLEANUP_TIMEOUT,
    environment: dict[str, str] | None = None,
    hold_signal_mask_on_return: bool = False,
) -> int:
    """Run selected cases sequentially with independent outer containment."""
    available = TESTS.keys() | RUNNER_PROBES.keys()
    unknown = [name for name in selected if name not in available]
    if unknown:
        print(f"unknown tests: {', '.join(unknown)}", file=sys.stderr)
        return 2

    guard = RunnerSignalGuard.create()
    failures = 0
    try:
        for name in selected:
            if guard.first_signal() is not None:
                break
            try:
                outcome = run_isolated_case(
                    name,
                    timeout=case_timeout,
                    cleanup_timeout=case_cleanup_timeout,
                    environment=environment,
                    signal_guard=guard,
                )
            except Exception:
                if guard.first_signal() is None:
                    failures += 1
                    print(
                        f"not ok {name}: outer containment failed",
                        file=sys.stderr,
                    )
                    traceback.print_exc()
                break
            if guard.first_signal() is not None:
                break
            if outcome.timed_out or outcome.status != 0:
                failures += 1
                print(
                    f"not ok {name}: status={outcome.status} "
                    f"timed_out={outcome.timed_out}",
                    file=sys.stderr,
                )
                if outcome.stdout:
                    print(f"case stdout:\n{outcome.stdout}", file=sys.stderr, end="")
                if outcome.stderr:
                    print(f"case stderr:\n{outcome.stderr}", file=sys.stderr, end="")
            else:
                print(f"ok {name} ({outcome.elapsed:.3f}s)")
    finally:
        final_signal = guard.finalize(hold_mask=hold_signal_mask_on_return)
    if final_signal is not None:
        return 128 + final_signal
    return 1 if failures else 0


def main(arguments: list[str]) -> int:
    """Run selected functional cases without requiring pytest."""
    if arguments[:1] == [INTERNAL_CASE_MODE]:
        return run_internal_case(arguments[1:])
    selected = arguments or list(TESTS)
    available = TESTS.keys() | RUNNER_PROBES.keys()
    unknown = [name for name in selected if name not in available]
    if unknown:
        print(f"unknown tests: {', '.join(unknown)}", file=sys.stderr)
        return 2
    return run_selected_cases(selected, hold_signal_mask_on_return=True)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
