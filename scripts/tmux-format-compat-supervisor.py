#!/usr/bin/env python3

"""Supervise and clean one Linux tmux compatibility worker lineage."""

from __future__ import annotations

import argparse
import contextlib
import ctypes
import dataclasses
import errno
import os
import secrets
import select
import signal
import stat
import sys
import time
import types
import typing as t

PR_SET_PDEATHSIG = 1
PR_SET_CHILD_SUBREAPER = 36
PR_GET_CHILD_SUBREAPER = 37
AT_SYMLINK_NOFOLLOW = 0x100
AT_EMPTY_PATH = 0x1000
STATX_MNT_ID = 0x1000
WAIT_ALL_CHILDREN = 0x40000000
RENAME_NOREPLACE = 0x1

ROOT_PARENT = "/tmp"
ROOT_PREFIX = "libtmux-tmux-format-compat."
ROOT_CLAIM_PREFIX = ".libtmux-tmux-format-compat.cleanup."
WORKER_ROOT_FD_ENV = "LIBTMUX_TFC_CASE_ROOT_PARENT_FD"
AFTER_ROOT_CREATE_CONTROL_ENV = "TFC_AFTER_ROOT_CREATE_CONTROL"
TEST_FINAL_CUTOVER_STOP_FD_ENV = "LIBTMUX_TFC_TEST_FINAL_CUTOVER_STOP_FD"
STARTUP_TIMEOUT = 5.0
TERM_TIMEOUT = 1.0
KILL_TIMEOUT = 2.0
POLL_INTERVAL = 0.01
WATCHED_SIGNALS = (signal.SIGHUP, signal.SIGINT, signal.SIGTERM)


class StatxTimestamp(ctypes.Structure):
    """Linux statx timestamp layout."""

    _fields_ = [
        ("seconds", ctypes.c_int64),
        ("nanoseconds", ctypes.c_uint32),
        ("reserved", ctypes.c_int32),
    ]


class Statx(ctypes.Structure):
    """Linux statx layout through the mount identity field."""

    _fields_ = [
        ("mask", ctypes.c_uint32),
        ("block_size", ctypes.c_uint32),
        ("attributes", ctypes.c_uint64),
        ("link_count", ctypes.c_uint32),
        ("uid", ctypes.c_uint32),
        ("gid", ctypes.c_uint32),
        ("mode", ctypes.c_uint16),
        ("spare0", ctypes.c_uint16 * 1),
        ("inode", ctypes.c_uint64),
        ("size", ctypes.c_uint64),
        ("blocks", ctypes.c_uint64),
        ("attributes_mask", ctypes.c_uint64),
        ("access_time", StatxTimestamp),
        ("birth_time", StatxTimestamp),
        ("change_time", StatxTimestamp),
        ("modify_time", StatxTimestamp),
        ("rdev_major", ctypes.c_uint32),
        ("rdev_minor", ctypes.c_uint32),
        ("dev_major", ctypes.c_uint32),
        ("dev_minor", ctypes.c_uint32),
        ("mount_id", ctypes.c_uint64),
        ("dio_memory_alignment", ctypes.c_uint32),
        ("dio_offset_alignment", ctypes.c_uint32),
        ("spare3", ctypes.c_uint64 * 12),
    ]


LIBC = ctypes.CDLL(None, use_errno=True)
LIBC.prctl.argtypes = (
    ctypes.c_int,
    ctypes.c_ulong,
    ctypes.c_ulong,
    ctypes.c_ulong,
    ctypes.c_ulong,
)
LIBC.prctl.restype = ctypes.c_int
LIBC.statx.argtypes = (
    ctypes.c_int,
    ctypes.c_char_p,
    ctypes.c_int,
    ctypes.c_uint,
    ctypes.POINTER(Statx),
)
LIBC.statx.restype = ctypes.c_int
LIBC.renameat2.argtypes = (
    ctypes.c_int,
    ctypes.c_char_p,
    ctypes.c_int,
    ctypes.c_char_p,
    ctypes.c_uint,
)
LIBC.renameat2.restype = ctypes.c_int

# Exact fail-closed diagnostics are constructed at their checks, and procfs plus
# descriptor-relative filesystem access intentionally use the lower-level os API.
# ruff: noqa: BLE001, EM101, EM102, PERF203, PTH115, PTH116, PTH117, PTH118, PTH123, PTH208, SIM105, TRY003, TRY300, TRY301


class SupervisorFailure(RuntimeError):
    """A failure that prevents lifecycle or deletion proof."""


def _mount_id(descriptor: int) -> int:
    """Return the Linux mount identity for an open descriptor."""
    query = Statx()
    result = LIBC.statx(
        descriptor,
        b"",
        AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW,
        STATX_MNT_ID,
        ctypes.byref(query),
    )
    if result != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number))
    if not query.mask & STATX_MNT_ID:
        raise SupervisorFailure("statx mount identity is unavailable")
    return int(query.mount_id)


def _rename_noreplace(
    source_parent_fd: int,
    source_name: str,
    destination_parent_fd: int,
    destination_name: str,
) -> None:
    """Atomically move one name without replacing an existing entry."""
    result = LIBC.renameat2(
        source_parent_fd,
        os.fsencode(source_name),
        destination_parent_fd,
        os.fsencode(destination_name),
        RENAME_NOREPLACE,
    )
    if result != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number))


@dataclasses.dataclass(frozen=True)
class ProcessSnapshot:
    """Stable fields parsed from one proc stat entry."""

    state: str
    parent_pid: int
    start_time: int


@dataclasses.dataclass
class TrackedProcess:
    """A kernel-lineage member held by pidfd until it is reaped."""

    pid: int
    pidfd: int
    start_time: int
    parent_identity: str
    reaped: bool = False
    wait_status: int | None = None
    stopped: bool = False
    termination_signal: int | None = None

    @property
    def identity(self) -> str:
        """Return a non-reusable diagnostic identity."""
        return f"pid={self.pid} start={self.start_time}"


class SignalLatch:
    """Latch the first external signal and wake the supervision loop."""

    def __init__(self) -> None:
        self.first: int | None = None
        self.count = 0
        self.read_fd, self.write_fd = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
        self.previous: dict[int, t.Any] = {}

    def _handler(self, signal_number: int, _frame: types.FrameType | None) -> None:
        if self.first is None:
            self.first = signal_number
        self.count += 1
        try:
            os.write(self.write_fd, b"s")
        except BlockingIOError:
            pass

    def install(self) -> None:
        """Install handlers before the worker can exist."""
        for signal_number in WATCHED_SIGNALS:
            self.previous[signal_number] = signal.getsignal(signal_number)
            signal.signal(signal_number, self._handler)

    def drain(self) -> None:
        """Drain wake bytes without changing the latched signal."""
        while True:
            try:
                if not os.read(self.read_fd, 4096):
                    return
            except BlockingIOError:
                return

    def restore(self) -> None:
        """Restore inherited dispositions and close the self-pipe."""
        for signal_number, handler in self.previous.items():
            signal.signal(signal_number, handler)
        os.close(self.read_fd)
        os.close(self.write_fd)


def _cutover_signal(latch: SignalLatch) -> int | None:
    """Choose the latched or pending first signal at the blocked cutover."""
    if latch.first is not None:
        return latch.first
    pending = signal.sigpending()
    for signal_number in WATCHED_SIGNALS:
        if signal_number in pending:
            return int(signal_number)
    return None


def _take_test_stop_fd(environment_name: str) -> int | None:
    """Consume one private, pipe-backed test capability before spawning."""
    raw_fd = os.environ.pop(environment_name, None)
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


def _test_stop_after_mask(descriptor: int | None) -> None:
    """Publish a one-shot test marker, close its capability, and self-stop."""
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


class BuildRoot:
    """A build directory owned through stable parent and root descriptors."""

    def __init__(
        self,
        parent_fd: int,
        name: str,
        root_fd: int,
        device: int,
        inode: int,
        mount_id: int,
        parent_path: str = ROOT_PARENT,
    ) -> None:
        self.parent_fd = parent_fd
        self.name = name
        self.root_fd = root_fd
        self.device = device
        self.inode = inode
        self.mount_id = mount_id
        self.parent_path = parent_path
        self.deleted = False

    @classmethod
    def create(cls, parent_fd: int | None = None) -> BuildRoot:
        """Create a mode-0700 root relative to an owned parent descriptor."""
        if parent_fd is None:
            parent_fd = os.open(
                ROOT_PARENT,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC,
            )
            parent_path = ROOT_PARENT
        else:
            if parent_fd <= 2:
                raise SupervisorFailure("invalid build-root parent descriptor")
            try:
                os.set_inheritable(parent_fd, False)
                parent = os.fstat(parent_fd)
                if not stat.S_ISDIR(parent.st_mode):
                    raise SupervisorFailure(
                        "build-root parent descriptor is not a directory"
                    )
                parent_path = os.readlink(f"/proc/self/fd/{parent_fd}")
                if not os.path.isabs(parent_path) or parent_path.endswith(" (deleted)"):
                    raise SupervisorFailure("build-root parent path is unavailable")
            except Exception:
                os.close(parent_fd)
                raise
        try:
            for _attempt in range(128):
                name = ROOT_PREFIX + secrets.token_hex(6)
                try:
                    os.mkdir(name, mode=0o700, dir_fd=parent_fd)
                except FileExistsError:
                    continue
                try:
                    root_fd = os.open(
                        name,
                        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                        dir_fd=parent_fd,
                    )
                except Exception:
                    os.rmdir(name, dir_fd=parent_fd)
                    raise
                try:
                    root_stat = os.fstat(root_fd)
                    root_mount_id = _mount_id(root_fd)
                except Exception:
                    os.close(root_fd)
                    os.rmdir(name, dir_fd=parent_fd)
                    raise
                return cls(
                    parent_fd,
                    name,
                    root_fd,
                    root_stat.st_dev,
                    root_stat.st_ino,
                    root_mount_id,
                    parent_path,
                )
        except Exception:
            os.close(parent_fd)
            raise
        os.close(parent_fd)
        raise SupervisorFailure("could not allocate compatibility build root")

    @property
    def path(self) -> str:
        """Return the worker-facing path to the held root."""
        return os.path.join(self.parent_path, self.name)

    @property
    def identity(self) -> str:
        """Return the held filesystem identity."""
        return f"dev={self.device} ino={self.inode}"

    def _same_identity(self, entry_stat: os.stat_result) -> bool:
        return (
            entry_stat.st_dev == self.device
            and entry_stat.st_ino == self.inode
            and stat.S_ISDIR(entry_stat.st_mode)
        )

    def _open_verified_root_name(self, name: str) -> int:
        named_fd: int | None = None
        try:
            named_fd = os.open(
                name,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=self.parent_fd,
            )
        except FileNotFoundError as error:
            raise SupervisorFailure(
                "refusing to delete replaced build root: name is missing"
            ) from error
        except OSError as error:
            raise SupervisorFailure(
                "refusing to delete replaced build root: name is not a directory"
            ) from error
        try:
            entry_stat = os.fstat(named_fd)
            if not self._same_identity(entry_stat):
                raise SupervisorFailure(
                    "refusing to delete replaced build root: creation identity changed"
                )
            if _mount_id(named_fd) != self.mount_id:
                raise SupervisorFailure(
                    "refusing to delete replaced build root: mount identity changed"
                )
            held_stat = os.fstat(self.root_fd)
            if not self._same_identity(held_stat):
                raise SupervisorFailure(
                    "refusing to delete replaced build root: descriptor changed"
                )
            if _mount_id(self.root_fd) != self.mount_id:
                raise SupervisorFailure(
                    "refusing to delete replaced build root: descriptor mount changed"
                )
            result = named_fd
            named_fd = None
            return result
        finally:
            if named_fd is not None:
                os.close(named_fd)

    def _verify_root_name(self) -> None:
        named_fd = self._open_verified_root_name(self.name)
        os.close(named_fd)

    def _claim_root_name(self) -> int:
        """Atomically quarantine and validate the creation-time root identity."""
        self._verify_root_name()
        source_name = self.name
        for _attempt in range(128):
            claim_name = ROOT_CLAIM_PREFIX + secrets.token_hex(6)
            try:
                _rename_noreplace(
                    self.parent_fd,
                    source_name,
                    self.parent_fd,
                    claim_name,
                )
            except FileExistsError:
                continue
            except OSError as error:
                raise SupervisorFailure(
                    "could not atomically claim build root for cleanup"
                ) from error

            try:
                claimed_fd = self._open_verified_root_name(claim_name)
            except Exception as error:
                try:
                    _rename_noreplace(
                        self.parent_fd,
                        claim_name,
                        self.parent_fd,
                        source_name,
                    )
                except OSError as restore_error:
                    raise SupervisorFailure(
                        "claimed build root identity changed and restore failed"
                    ) from restore_error
                raise SupervisorFailure(
                    "claimed build root identity changed before cleanup"
                ) from error

            # Owned process closure is proved before callers reach deletion.
            # An unrelated malicious same-fsuid namespace mutator is outside the
            # harness guarantee; any drift observed at this atomic boundary is
            # retained rather than traversed.
            self.name = claim_name
            return claimed_fd
        raise SupervisorFailure("could not allocate build-root cleanup claim")

    def _purge_directory(self, directory_fd: int) -> None:
        # The whole root has already been claimed after owned-lineage closure,
        # so descendants are no longer reachable by any supported mutator.
        for name in os.listdir(directory_fd):
            before = os.stat(
                name,
                dir_fd=directory_fd,
                follow_symlinks=False,
            )
            if stat.S_ISDIR(before.st_mode):
                if before.st_dev != self.device:
                    raise SupervisorFailure(
                        "refusing to cross a filesystem while deleting build root"
                    )
                child_fd = os.open(
                    name,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                    dir_fd=directory_fd,
                )
                try:
                    opened = os.fstat(child_fd)
                    if opened.st_dev != before.st_dev or opened.st_ino != before.st_ino:
                        raise SupervisorFailure(
                            "refusing changed directory in build root"
                        )
                    if _mount_id(child_fd) != self.mount_id:
                        raise SupervisorFailure(
                            "refusing to cross a mount while deleting build root"
                        )
                    self._purge_directory(child_fd)
                    current = os.stat(
                        name,
                        dir_fd=directory_fd,
                        follow_symlinks=False,
                    )
                    if (
                        current.st_dev != opened.st_dev
                        or current.st_ino != opened.st_ino
                        or not stat.S_ISDIR(current.st_mode)
                    ):
                        raise SupervisorFailure(
                            "refusing changed directory in build root"
                        )
                    os.rmdir(name, dir_fd=directory_fd)
                finally:
                    os.close(child_fd)
            else:
                current = os.stat(
                    name,
                    dir_fd=directory_fd,
                    follow_symlinks=False,
                )
                if (
                    current.st_dev != before.st_dev
                    or current.st_ino != before.st_ino
                    or current.st_mode != before.st_mode
                ):
                    raise SupervisorFailure("refusing changed entry in build root")
                os.unlink(name, dir_fd=directory_fd)

    def _validate_directory_tree(self, directory_fd: int) -> None:
        """Reject mount crossings or identity drift before deleting any entry."""
        for name in os.listdir(directory_fd):
            before = os.stat(
                name,
                dir_fd=directory_fd,
                follow_symlinks=False,
            )
            entry_fd = os.open(
                name,
                os.O_PATH | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=directory_fd,
            )
            try:
                opened = os.fstat(entry_fd)
                if (
                    opened.st_dev != before.st_dev
                    or opened.st_ino != before.st_ino
                    or opened.st_mode != before.st_mode
                ):
                    raise SupervisorFailure("refusing changed entry in build root")
                if opened.st_dev != self.device:
                    raise SupervisorFailure(
                        "refusing to cross a filesystem while deleting build root"
                    )
                if _mount_id(entry_fd) != self.mount_id:
                    raise SupervisorFailure(
                        "refusing to cross a mount while deleting build root"
                    )
                if stat.S_ISDIR(opened.st_mode):
                    child_fd = os.open(
                        name,
                        os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                        dir_fd=directory_fd,
                    )
                    try:
                        child = os.fstat(child_fd)
                        if (
                            child.st_dev != opened.st_dev
                            or child.st_ino != opened.st_ino
                            or child.st_mode != opened.st_mode
                        ):
                            raise SupervisorFailure(
                                "refusing changed directory in build root"
                            )
                        if _mount_id(child_fd) != self.mount_id:
                            raise SupervisorFailure(
                                "refusing to cross a mount while deleting build root"
                            )
                        self._validate_directory_tree(child_fd)
                    finally:
                        os.close(child_fd)
            finally:
                os.close(entry_fd)
            current = os.stat(
                name,
                dir_fd=directory_fd,
                follow_symlinks=False,
            )
            if (
                current.st_dev != before.st_dev
                or current.st_ino != before.st_ino
                or current.st_mode != before.st_mode
            ):
                raise SupervisorFailure("refusing changed entry in build root")

    def delete(self) -> None:
        """Delete only the still-named root through held descriptors."""
        claimed_fd = self._claim_root_name()
        try:
            self._validate_directory_tree(claimed_fd)
            self._purge_directory(claimed_fd)
            self._verify_root_name()
            os.rmdir(self.name, dir_fd=self.parent_fd)
            self.deleted = True
        finally:
            os.close(claimed_fd)

    def retained_location(self) -> str:
        """Return the current kernel rendering of the held root descriptor."""
        try:
            return os.readlink(f"/proc/self/fd/{self.root_fd}")
        except OSError:
            return self.path

    def close(self) -> None:
        """Close ownership descriptors after delete or retention reporting."""
        os.close(self.root_fd)
        os.close(self.parent_fd)


def _prctl(option: int, argument: int) -> None:
    result = LIBC.prctl(option, argument, 0, 0, 0)
    if result != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number))


def _enable_and_verify_subreaper() -> None:
    _prctl(PR_SET_CHILD_SUBREAPER, 1)
    state = ctypes.c_int()
    state_address = ctypes.cast(ctypes.byref(state), ctypes.c_void_p).value
    if state_address is None:
        raise SupervisorFailure("child subreaper state address is unavailable")
    result = LIBC.prctl(PR_GET_CHILD_SUBREAPER, state_address, 0, 0, 0)
    if result != 0:
        error_number = ctypes.get_errno()
        raise OSError(error_number, os.strerror(error_number))
    if state.value != 1:
        raise SupervisorFailure("child subreaper verification failed")


def _read_stat_file(path: str) -> ProcessSnapshot:
    with open(path, "rb", buffering=0) as stat_file:
        data = stat_file.read()
    command_end = data.rfind(b") ")
    if command_end < 0:
        raise SupervisorFailure("malformed proc stat entry")
    fields = data[command_end + 2 :].split()
    if len(fields) < 20:
        raise SupervisorFailure("short proc stat entry")
    try:
        return ProcessSnapshot(
            fields[0].decode("ascii"),
            int(fields[1]),
            int(fields[19]),
        )
    except (UnicodeDecodeError, ValueError) as error:
        raise SupervisorFailure("invalid proc stat entry") from error


def _process_snapshot(pid: int) -> ProcessSnapshot:
    return _read_stat_file(f"/proc/{pid}/stat")


def _pidfd_exited(pidfd: int) -> bool:
    poller = select.poll()
    poller.register(pidfd, select.POLLIN | select.POLLHUP | select.POLLERR)
    return bool(poller.poll(0))


def _send_pidfd(process: TrackedProcess, signal_number: int) -> None:
    if process.reaped or _pidfd_exited(process.pidfd):
        return
    try:
        signal.pidfd_send_signal(process.pidfd, signal_number)
    except ProcessLookupError:
        return
    except OSError as error:
        if error.errno == errno.ESRCH:
            return
        raise SupervisorFailure(
            f"pidfd signal failed for {process.identity}: errno={error.errno}"
        ) from error


def _waitpid(pid: int, options: int) -> tuple[int, int]:
    """Keep all child-state consumption in one primitive."""
    return os.waitpid(pid, options)


def _wait_status(status: int) -> int:
    if os.WIFEXITED(status):
        return os.WEXITSTATUS(status)
    if os.WIFSIGNALED(status):
        return 128 + os.WTERMSIG(status)
    raise SupervisorFailure("worker has no terminal wait status")


class Supervisor:
    """Own one Bash worker and its complete kernel descendant lineage."""

    def __init__(
        self,
        command: list[str],
        root: BuildRoot,
        latch: SignalLatch,
        test_fault: str | None,
        pass_root_fd_to_worker: bool = False,
    ) -> None:
        self.command = command
        self.root = root
        self.latch = latch
        self.test_fault = test_fault
        self.pass_root_fd_to_worker = pass_root_fd_to_worker
        self.pid = os.getpid()
        self.processes: dict[int, TrackedProcess] = {}
        self.worker: TrackedProcess | None = None
        self.untracked_reaped: list[int] = []

    def _new_process(
        self,
        pid: int,
        parent_identity: str,
        *,
        require_parent_pid: int | None = None,
    ) -> TrackedProcess | None:
        existing = self.processes.get(pid)
        if existing is not None and not existing.reaped:
            return existing
        if existing is not None:
            del self.processes[pid]
        try:
            pidfd = os.pidfd_open(pid)
        except ProcessLookupError:
            return None
        try:
            try:
                snapshot = _process_snapshot(pid)
            except FileNotFoundError:
                if _pidfd_exited(pidfd):
                    os.close(pidfd)
                    return None
                raise
            if (
                require_parent_pid is not None
                and snapshot.parent_pid != require_parent_pid
            ):
                raise SupervisorFailure(
                    f"kernel lineage changed before registration: pid={pid} "
                    f"parent={snapshot.parent_pid} expected={require_parent_pid}"
                )
            process = TrackedProcess(
                pid,
                pidfd,
                snapshot.start_time,
                parent_identity,
            )
            self.processes[pid] = process
            return process
        except Exception:
            os.close(pidfd)
            raise

    def _child_exec(
        self,
        gate_read_fd: int,
        gate_write_fd: int,
        inherited_mask: set[int | signal.Signals],
    ) -> t.NoReturn:
        try:
            os.close(gate_write_fd)
            _prctl(PR_SET_PDEATHSIG, signal.SIGKILL)
            if os.getppid() != self.pid:
                os._exit(125)
            os.setsid()
            if os.read(gate_read_fd, 1) != b"g":
                os._exit(125)
            os.close(gate_read_fd)
            for signal_number in WATCHED_SIGNALS:
                signal.signal(signal_number, signal.SIG_DFL)
            signal.signal(signal.SIGCHLD, signal.SIG_DFL)
            signal.pthread_sigmask(signal.SIG_SETMASK, inherited_mask)
            environment = os.environ.copy()
            environment.pop("BASH_ENV", None)
            environment.pop("ENV", None)
            environment.pop(WORKER_ROOT_FD_ENV, None)
            if self.pass_root_fd_to_worker:
                worker_root_fd = os.dup(self.root.root_fd)
                os.set_inheritable(worker_root_fd, True)
                environment[WORKER_ROOT_FD_ENV] = str(worker_root_fd)
            os.execve(
                self.command[0],
                [*self.command, self.root.path],
                environment,
            )
        except BaseException as error:
            message = f"compatibility worker exec failed: {error}\n".encode(
                "utf-8", "backslashreplace"
            )
            try:
                os.write(2, message)
            except OSError:
                pass
            os._exit(125)

    def spawn_stopped_worker(
        self, inherited_mask: set[int | signal.Signals]
    ) -> TrackedProcess:
        """Fork, pidfd-register, and verify one self-stopped Bash worker."""
        expected_executable = os.stat(self.command[0])
        gate_read_fd, gate_write_fd = os.pipe2(os.O_CLOEXEC)
        try:
            pid = os.fork()
        except Exception:
            os.close(gate_read_fd)
            os.close(gate_write_fd)
            raise
        if pid == 0:
            self._child_exec(gate_read_fd, gate_write_fd, inherited_mask)

        os.close(gate_read_fd)
        try:
            worker = self._new_process(
                pid,
                f"supervisor={self.pid}",
                require_parent_pid=self.pid,
            )
            if worker is None:
                raise SupervisorFailure("worker vanished before pidfd registration")
            self.worker = worker
            os.write(gate_write_fd, b"g")
        finally:
            os.close(gate_write_fd)

        deadline = time.monotonic() + STARTUP_TIMEOUT
        while time.monotonic() < deadline:
            waited_pid, wait_status = _waitpid(
                worker.pid,
                os.WNOHANG | os.WUNTRACED | WAIT_ALL_CHILDREN,
            )
            if waited_pid == 0:
                self._wait_activity(deadline)
                continue
            if not os.WIFSTOPPED(wait_status):
                self._record_terminal(worker, wait_status)
                raise SupervisorFailure(
                    "compatibility worker exited before startup stop"
                )
            if os.WSTOPSIG(wait_status) != signal.SIGSTOP:
                raise SupervisorFailure(
                    "compatibility worker used an unexpected startup stop"
                )
            worker.stopped = True
            actual_executable = os.stat(f"/proc/{worker.pid}/exe")
            if (
                actual_executable.st_dev != expected_executable.st_dev
                or actual_executable.st_ino != expected_executable.st_ino
            ):
                raise SupervisorFailure(
                    "stopped compatibility worker is not the requested Bash"
                )
            self._confirm_stopped(worker, deadline)
            return worker
        raise SupervisorFailure("compatibility worker did not enter startup stop")

    def _record_terminal(self, process: TrackedProcess, wait_status: int) -> None:
        process.wait_status = wait_status
        process.reaped = True
        process.stopped = False
        os.close(process.pidfd)

    def _reap_available(self) -> bool:
        """Reap every available child and return whether waitpid proved ECHILD."""
        while True:
            try:
                pid, wait_status = _waitpid(
                    -1,
                    os.WNOHANG | WAIT_ALL_CHILDREN,
                )
            except ChildProcessError:
                return True
            except OSError as error:
                if error.errno == errno.ECHILD:
                    return True
                if error.errno == errno.EINTR:
                    continue
                raise SupervisorFailure(
                    f"waitpid failed: errno={error.errno}"
                ) from error
            if pid == 0:
                return False
            process = self.processes.get(pid)
            if process is None:
                self.untracked_reaped.append(pid)
                continue
            self._record_terminal(process, wait_status)

    def _wait_activity(self, deadline: float) -> None:
        timeout = max(0.0, min(POLL_INTERVAL, deadline - time.monotonic()))
        descriptors = [self.latch.read_fd]
        if self.worker is not None and not self.worker.reaped:
            descriptors.append(self.worker.pidfd)
        select.select(descriptors, [], [], timeout)
        self.latch.drain()

    def wait_for_worker_or_signal(self) -> None:
        """Wait until the worker terminates or the first signal is latched."""
        if self.worker is None:
            raise SupervisorFailure("compatibility worker is unavailable")
        while self.worker.wait_status is None and self.latch.first is None:
            self._reap_available()
            if self.worker.wait_status is not None:
                return
            self._wait_activity(time.monotonic() + POLL_INTERVAL)

    def _task_states(self, process: TrackedProcess) -> dict[int, str]:
        task_root = f"/proc/{process.pid}/task"
        tids = sorted(int(name) for name in os.listdir(task_root) if name.isdigit())
        states: dict[int, str] = {}
        for tid in tids:
            snapshot = _read_stat_file(f"{task_root}/{tid}/stat")
            if tid == process.pid and snapshot.start_time != process.start_time:
                raise SupervisorFailure(f"proc identity changed for {process.identity}")
            states[tid] = snapshot.state
        return states

    def _confirm_stopped(self, process: TrackedProcess, deadline: float) -> bool:
        while time.monotonic() < deadline:
            if process.reaped or _pidfd_exited(process.pidfd):
                process.stopped = False
                return False
            try:
                first = self._task_states(process)
                if first and all(state in {"T", "t"} for state in first.values()):
                    second = self._task_states(process)
                    if first == second:
                        process.stopped = True
                        return True
            except FileNotFoundError:
                if _pidfd_exited(process.pidfd):
                    process.stopped = False
                    return False
            self._wait_activity(deadline)
        states = "unknown"
        try:
            states = ",".join(
                f"{tid}:{state}" for tid, state in self._task_states(process).items()
            )
        except OSError:
            pass
        raise SupervisorFailure(
            f"process stop deadline expired for {process.identity} states={states}"
        )

    def _freeze(self, process: TrackedProcess, deadline: float) -> bool:
        if process.reaped or _pidfd_exited(process.pidfd):
            process.stopped = False
            return False
        _send_pidfd(process, signal.SIGSTOP)
        return self._confirm_stopped(process, deadline)

    def _children_of(self, process: TrackedProcess) -> set[int]:
        if self.test_fault == "worker-children-unavailable":
            raise SupervisorFailure("injected worker children failure")
        if not process.stopped:
            raise SupervisorFailure(
                f"refusing an unfrozen children snapshot for {process.identity}"
            )
        task_root = f"/proc/{process.pid}/task"
        before = self._task_states(process)
        if not before or any(state not in {"T", "t"} for state in before.values()):
            raise SupervisorFailure(
                f"process changed while reading children for {process.identity}"
            )
        children: set[int] = set()
        for tid in sorted(before):
            with open(f"{task_root}/{tid}/children", "rb", buffering=0) as source:
                payload = source.read()
            for raw_pid in payload.split():
                if not raw_pid.isdigit() or int(raw_pid) <= 0:
                    raise SupervisorFailure("invalid proc children entry")
                children.add(int(raw_pid))
        after = self._task_states(process)
        if before != after:
            raise SupervisorFailure(
                f"process changed while reading children for {process.identity}"
            )
        return children

    def _supervisor_children(self) -> set[int]:
        if self.test_fault == "supervisor-children-unavailable":
            raise SupervisorFailure("injected supervisor children failure")
        children: set[int] = set()
        task_root = "/proc/self/task"
        for name in os.listdir(task_root):
            if not name.isdigit():
                continue
            try:
                with open(f"{task_root}/{name}/children", "rb", buffering=0) as source:
                    payload = source.read()
            except FileNotFoundError:
                continue
            for raw_pid in payload.split():
                if not raw_pid.isdigit() or int(raw_pid) <= 0:
                    raise SupervisorFailure("invalid supervisor children entry")
                children.add(int(raw_pid))
        return children

    def _discover_supervisor_children(self) -> bool:
        changed = False
        for child_pid in self._supervisor_children():
            existing = self.processes.get(child_pid)
            if existing is not None and not existing.reaped:
                continue
            process = self._new_process(
                child_pid,
                f"supervisor={self.pid}",
                require_parent_pid=self.pid,
            )
            changed = changed or process is not None
        return changed

    def preflight_cleanup_observation(self, worker: TrackedProcess) -> None:
        """Prove procfs can stably enumerate the stopped initial lineage."""
        if not worker.stopped:
            raise SupervisorFailure("cleanup preflight requires a stopped worker")
        supervisor_children_before = self._supervisor_children()
        worker_children = self._children_of(worker)
        supervisor_children_after = self._supervisor_children()
        if supervisor_children_before != supervisor_children_after:
            raise SupervisorFailure(
                "supervisor children changed during cleanup preflight"
            )
        if supervisor_children_after != {worker.pid}:
            raise SupervisorFailure(
                "cleanup preflight found an unexpected supervisor child set"
            )
        if worker_children:
            raise SupervisorFailure(
                "cleanup preflight found unexpected stopped-worker children"
            )

    def _freeze_fixed_point(self, deadline: float) -> None:
        if self.test_fault == "freeze":
            raise SupervisorFailure("injected kernel-lineage freeze failure")
        while time.monotonic() < deadline:
            changed = self._discover_supervisor_children()
            for process in list(self.processes.values()):
                if process.reaped:
                    continue
                self._freeze(process, deadline)
            for parent in list(self.processes.values()):
                if parent.reaped or not parent.stopped:
                    continue
                try:
                    child_pids = self._children_of(parent)
                except FileNotFoundError:
                    if _pidfd_exited(parent.pidfd):
                        continue
                    raise
                for child_pid in child_pids:
                    existing = self.processes.get(child_pid)
                    if existing is not None and not existing.reaped:
                        continue
                    child = self._new_process(
                        child_pid,
                        parent.identity,
                        require_parent_pid=parent.pid,
                    )
                    changed = changed or child is not None
            changed = self._discover_supervisor_children() or changed
            if changed:
                continue
            for process in self.processes.values():
                if process.reaped or _pidfd_exited(process.pidfd):
                    continue
                if not process.stopped:
                    break
                self._children_of(process)
            else:
                if not self._discover_supervisor_children():
                    return
        raise SupervisorFailure("kernel-lineage freeze deadline expired")

    def _signal_frozen_lineage(self, signal_number: int) -> None:
        for process in self.processes.values():
            if process.reaped or _pidfd_exited(process.pidfd):
                continue
            if process.termination_signal is None:
                _send_pidfd(process, signal_number)
                process.termination_signal = signal_number
        for process in self.processes.values():
            if process.reaped or not process.stopped:
                continue
            _send_pidfd(process, signal.SIGCONT)
            process.stopped = False

    def _kill_frozen_lineage(self) -> None:
        for process in self.processes.values():
            if process.reaped or _pidfd_exited(process.pidfd):
                continue
            _send_pidfd(process, signal.SIGKILL)

    def _closure_proved(self) -> bool:
        self._discover_supervisor_children()
        no_children = self._reap_available()
        self._discover_supervisor_children()
        if not no_children:
            return False
        if self.test_fault == "proc-children":
            raise SupervisorFailure("injected proc children failure")
        children = self._supervisor_children()
        if children:
            self._discover_supervisor_children()
            return False
        return True

    def _kill_closure(self, deadline: float) -> bool:
        """Repeatedly adopt, pidfd-kill, and all-child-reap to closure."""
        observation_failure: Exception | None = None
        while True:
            try:
                self._discover_supervisor_children()
            except (OSError, SupervisorFailure) as error:
                observation_failure = error
            self._kill_frozen_lineage()
            self._reap_available()
            try:
                self._discover_supervisor_children()
                if self._closure_proved():
                    return True
            except (OSError, SupervisorFailure) as error:
                observation_failure = error
            if time.monotonic() >= deadline:
                break
            self._wait_activity(deadline)
        if observation_failure is not None:
            raise observation_failure
        return False

    def terminate_lineage(self) -> bool:
        """Apply absolute TERM/KILL deadlines and prove closure with ECHILD."""
        term_deadline = time.monotonic() + TERM_TIMEOUT
        graceful_failure: Exception | None = None

        while time.monotonic() < term_deadline:
            try:
                if self._closure_proved():
                    return True
            except (OSError, SupervisorFailure) as error:
                graceful_failure = error
                break
            if self.latch.count > 1:
                break
            try:
                self._freeze_fixed_point(term_deadline)
            except (OSError, SupervisorFailure) as error:
                graceful_failure = error
                break
            initiating_signal = self.latch.first or signal.SIGTERM
            self._signal_frozen_lineage(initiating_signal)
            self._wait_activity(term_deadline)

        kill_deadline = time.monotonic() + KILL_TIMEOUT
        closure_proved = self._kill_closure(kill_deadline)
        if not closure_proved and graceful_failure is not None:
            raise graceful_failure
        return closure_proved

    def residue(self) -> list[str]:
        """Describe every exact tracked identity not known to be reaped."""
        result = []
        for process in self.processes.values():
            if process.reaped:
                continue
            state = "exited" if _pidfd_exited(process.pidfd) else "unknown"
            try:
                snapshot = _process_snapshot(process.pid)
            except (FileNotFoundError, OSError, SupervisorFailure):
                pass
            else:
                if snapshot.start_time == process.start_time:
                    state = snapshot.state
            result.append(
                f"{process.identity} state={state} via={process.parent_identity}"
            )
        known = set(self.processes)
        try:
            for pid in self._supervisor_children() - known:
                try:
                    snapshot = _process_snapshot(pid)
                except (FileNotFoundError, OSError, SupervisorFailure):
                    result.append(f"pid={pid} start=unknown state=unknown")
                else:
                    result.append(
                        f"pid={pid} start={snapshot.start_time} "
                        f"state={snapshot.state} via=supervisor={self.pid}"
                    )
        except (OSError, SupervisorFailure) as error:
            result.append(f"proc-children-unavailable={error}")
        return result

    def close_pidfds(self) -> None:
        """Close pidfds after proof or final residue reporting."""
        for process in self.processes.values():
            if process.reaped:
                continue
            os.close(process.pidfd)


def _validate_command(command: list[str]) -> None:
    if not command:
        raise SupervisorFailure("compatibility supervisor requires a worker")
    if not os.path.isabs(command[0]):
        raise SupervisorFailure("compatibility worker executable must be absolute")
    if not os.access(command[0], os.X_OK):
        raise SupervisorFailure("compatibility worker executable is unavailable")
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise SupervisorFailure("compatibility supervisor requires pidfd support")
    probe = os.pidfd_open(os.getpid())
    os.close(probe)


def _decode_watched_mask(raw_mask: int | None) -> set[signal.Signals] | None:
    """Validate a runner-provided mask for the watched signals only."""
    if raw_mask is None:
        return None
    allowed_mask = sum(1 << int(signal_number) for signal_number in WATCHED_SIGNALS)
    if raw_mask < 0 or raw_mask & ~allowed_mask:
        raise SupervisorFailure("invalid inherited watched-signal mask")
    return {
        signal_number
        for signal_number in WATCHED_SIGNALS
        if raw_mask & (1 << int(signal_number))
    }


def _arguments(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument(
        "--test-fault",
        choices=(
            "freeze",
            "proc-children",
            "supervisor-children-unavailable",
            "worker-children-unavailable",
            "after-root-create",
        ),
        default=None,
        help=argparse.SUPPRESS,
    )
    parser.add_argument(
        "--root-parent-fd",
        type=int,
        default=None,
        help=argparse.SUPPRESS,
    )
    parser.add_argument(
        "--pass-root-fd-to-worker",
        action="store_true",
        help=argparse.SUPPRESS,
    )
    parser.add_argument(
        "--inherited-watched-mask",
        type=int,
        default=None,
        help=argparse.SUPPRESS,
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    parsed = parser.parse_args(arguments)
    if parsed.command[:1] == ["--"]:
        parsed.command = parsed.command[1:]
    return parsed


def _run(arguments: list[str]) -> int:
    parsed = _arguments(arguments)
    _validate_command(parsed.command)
    inherited_watched = _decode_watched_mask(parsed.inherited_watched_mask)
    if sys.platform != "linux":
        raise SupervisorFailure("compatibility supervisor requires Linux")
    final_cutover_stop_fd = _take_test_stop_fd(TEST_FINAL_CUTOVER_STOP_FD_ENV)

    _enable_and_verify_subreaper()
    old_sigchld = signal.getsignal(signal.SIGCHLD)
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    latch = SignalLatch()
    latch.install()
    if inherited_watched is not None:
        launch_mask = signal.pthread_sigmask(signal.SIG_BLOCK, ())
        launch_mask.difference_update(WATCHED_SIGNALS)
        launch_mask.update(inherited_watched)
        signal.pthread_sigmask(signal.SIG_SETMASK, launch_mask)
    inherited_mask = signal.pthread_sigmask(signal.SIG_BLOCK, WATCHED_SIGNALS)
    root: BuildRoot | None = None
    supervisor: Supervisor | None = None
    failure: Exception | None = None
    closure_proved = False
    status = 1

    try:
        root = BuildRoot.create(parsed.root_parent_fd)
        if parsed.test_fault == "after-root-create":
            control = os.environ.get(AFTER_ROOT_CREATE_CONTROL_ENV)
            if control is None:
                raise SupervisorFailure("after-root-create control is unavailable")
            record_fd = os.open(
                os.path.join(control, "after-root-create-root"),
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC,
                0o600,
            )
            try:
                os.write(record_fd, root.path.encode("utf-8"))
            finally:
                os.close(record_fd)
            os.kill(os.getpid(), signal.SIGSTOP)
            raise SupervisorFailure("after-root-create fault resumed unexpectedly")
        supervisor = Supervisor(
            parsed.command,
            root,
            latch,
            parsed.test_fault,
            parsed.pass_root_fd_to_worker,
        )
        worker = supervisor.spawn_stopped_worker(inherited_mask)
        supervisor.preflight_cleanup_observation(worker)
        signal.pthread_sigmask(signal.SIG_SETMASK, inherited_mask)
        if latch.first is None:
            _send_pidfd(worker, signal.SIGCONT)
            worker.stopped = False
            supervisor.wait_for_worker_or_signal()
        closure_proved = supervisor.terminate_lineage()
        if not closure_proved:
            raise SupervisorFailure("process closure deadline expired")
        root.delete()
        if worker.wait_status is not None:
            status = _wait_status(worker.wait_status)
        else:
            raise SupervisorFailure("worker status is unavailable")
    except Exception as error:
        failure = error
        signal.pthread_sigmask(signal.SIG_SETMASK, inherited_mask)
        if supervisor is not None and not closure_proved:
            try:
                closure_proved = supervisor.terminate_lineage()
            except Exception as cleanup_error:
                print(
                    f"compatibility process cleanup failed: {cleanup_error}",
                    file=sys.stderr,
                )
        status = 1
    finally:
        signal.pthread_sigmask(signal.SIG_BLOCK, WATCHED_SIGNALS)
        _test_stop_after_mask(final_cutover_stop_fd)
        if failure is not None:
            print(str(failure), file=sys.stderr)
        if supervisor is not None and not closure_proved:
            residue = supervisor.residue()
            if residue:
                for identity in residue:
                    print(
                        f"unproven compatibility process residue: {identity}",
                        file=sys.stderr,
                    )
            else:
                print(
                    "compatibility process closure remains unproven",
                    file=sys.stderr,
                )
        if root is not None and not root.deleted:
            print(
                f"retained build root: path={root.retained_location()} {root.identity}",
                file=sys.stderr,
            )
        if supervisor is not None:
            supervisor.close_pidfds()
        if root is not None:
            root.close()
        final_signal = _cutover_signal(latch)
        if final_signal is not None:
            status = 128 + final_signal
        latch.restore()
        signal.signal(signal.SIGCHLD, old_sigchld)
    return status


def main(arguments: list[str]) -> int:
    """Run the Linux-only compatibility harness supervisor."""
    try:
        return _run(arguments)
    except Exception as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
