"""JSONL control bridge to a real Unix PTY; terminal interpretation belongs to xterm."""

import base64
import fcntl
import json
import os
import pty
import selectors
import signal
import struct
import sys
import termios
import time


def emit(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


config = json.loads(sys.stdin.buffer.readline())
pid, master = pty.fork()
if pid == 0:
    os.chdir(config["cwd"])
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", config["rows"], config["cols"], 0, 0))
    os.execvpe(config["command"][0], config["command"], config["env"])

os.set_blocking(master, False)
os.set_blocking(sys.stdin.fileno(), False)
selector = selectors.DefaultSelector()
selector.register(master, selectors.EVENT_READ, "pty")
selector.register(sys.stdin.fileno(), selectors.EVENT_READ, "control")
pending = b""
stop_at = None
emit({"type": "ready", "pid": pid})


def terminate(signum):
    try:
        os.killpg(pid, signum)
    except ProcessLookupError:
        pass


try:
    while True:
        for key, _ in selector.select(0.05):
            if key.data == "pty":
                try:
                    output = os.read(master, 65536)
                except OSError:
                    output = b""
                if output:
                    emit({"type": "data", "data": base64.b64encode(output).decode("ascii")})
                else:
                    selector.unregister(master)
            else:
                data = os.read(sys.stdin.fileno(), 65536)
                if not data:
                    selector.unregister(sys.stdin.fileno())
                    terminate(signal.SIGTERM)
                    stop_at = time.monotonic() + 1
                pending += data
                while b"\n" in pending:
                    line, pending = pending.split(b"\n", 1)
                    command = json.loads(line)
                    if command["type"] == "input":
                        value = base64.b64decode(command["data"])
                        while value:
                            written = os.write(master, value)
                            value = value[written:]
                    elif command["type"] == "resize":
                        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", command["rows"], command["cols"], 0, 0))
                        emit({"type": "resized", "id": command["id"], "cols": command["cols"], "rows": command["rows"]})
                    elif command["type"] == "stop":
                        terminate(signal.SIGTERM)
                        stop_at = time.monotonic() + 1
                    else:
                        raise ValueError("unknown control message")
        ended, status = os.waitpid(pid, os.WNOHANG)
        if ended:
            # Read the last bytes before reporting process exit.
            try:
                while True:
                    output = os.read(master, 65536)
                    if not output:
                        break
                    emit({"type": "data", "data": base64.b64encode(output).decode("ascii")})
            except OSError:
                pass
            emit({"type": "exit", "code": os.waitstatus_to_exitcode(status)})
            break
        if stop_at is not None and time.monotonic() >= stop_at:
            terminate(signal.SIGKILL)
            stop_at = None
finally:
    terminate(signal.SIGKILL)
    selector.close()
    os.close(master)
