"""Run one CLI handoff under an owned controlling terminal."""
import errno
import fcntl
import os
import pty
import select
import signal
import subprocess
import sys
import termios
import time


def terminal():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


hold = sys.argv[1:2] == ["--hold"]
argv = sys.argv[2:] if hold else sys.argv[1:]
master, slave = pty.openpty()
child = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave,
                         preexec_fn=terminal)
os.close(slave)
output = bytearray()
try:
    deadline = time.monotonic() + 5
    while hold or time.monotonic() < deadline:
        ready, _, _ = select.select([master, 0] if hold else [master], [], [], 0.05)
        if hold and 0 in ready and not os.read(0, 1):
            break
        if master in ready:
            try:
                data = os.read(master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not data:
                break
            if not hold:
                output.extend(data)
        elif child.poll() is not None:
            break
    if child.poll() is None:
        child.kill()
    status = child.wait(timeout=2)
finally:
    os.close(master)
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
sys.stderr.buffer.write(output)
raise SystemExit(status if status >= 0 else 128 - status)
