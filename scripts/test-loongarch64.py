#!/usr/bin/env python3
"""Check the ELF contract and exercise the built binary under LoongArch emulation."""

import argparse
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import time

# The distribution is statically linked, so a user-mode emulator alone is enough:
# there is no interpreter or shared library to resolve at run time.
QEMU = "qemu-loongarch64-static"
EM_LOONGARCH = 258


def check_elf(binary):
    data = binary.read_bytes()
    assert data[:7] == b"\x7fELF\x02\x01\x01", "Expected little-endian ELF64"
    header = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
    assert header[0] == 2 and header[1] == EM_LOONGARCH, "Expected LoongArch ET_EXEC"
    offset, entry_size, count = header[4], header[8], header[9]
    assert entry_size == 56 and count > 0, "Invalid ELF program headers"
    types = [struct.unpack_from("<I", data, offset + i * entry_size)[0]
             for i in range(count)]
    assert 1 in types, "Missing LOAD segment"
    assert 2 not in types and 3 not in types, "Dynamic segment or ELF interpreter found"
    print("PASS: ELF64 LoongArch, static executable, no interpreter/dynamic segment")


def check_runtime(binary, qemu):
    with tempfile.TemporaryDirectory(prefix="peri-loongarch64-") as temporary:
        fixture = Path(temporary)
        (fixture / "home").mkdir()
        (fixture / "settings.json").write_text(json.dumps({"config": {
            "active_alias": "sonnet",
            "providers": [{"id": "smoke", "type": "openai",
                           "apiKey": "not-a-credential"}],
            "profiles": {"sonnet": {"provider": "smoke", "model": "test-model"}},
        }}))
        command = [qemu, str(binary)]
        environment = dict(os.environ, HOME=str(fixture / "home"))
        for args, code, marker in [
            (["--version"], 0, "peri "),
            (["--help"], 0, "Usage:"),
            (["--invalid-loongarch64-smoke-option"], 2, "unexpected argument"),
        ]:
            result = subprocess.run(command + args, input="", text=True,
                                    capture_output=True, timeout=60, env=environment)
            output = result.stdout + result.stderr
            assert result.returncode == code, (args, result.returncode, output)
            assert marker in output, (args, output)
            print(f"PASS: {' '.join(args)} (exit {code})")

        with (fixture / "stderr.log").open("w+") as stderr:
            process = subprocess.Popen(
                command + ["--config-file", str(fixture / "settings.json"),
                           "--db-path", str(fixture / "threads.db"),
                           "acp", "--cwd", str(fixture)],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                text=True, env=environment,
            )
            messages = queue.Queue()

            def read_output():
                for line in process.stdout:
                    messages.put(line)
                messages.put(None)

            reader = threading.Thread(target=read_output, daemon=True)
            reader.start()

            def rpc(request_id, method, params):
                process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id,
                                                "method": method, "params": params}) + "\n")
                process.stdin.flush()
                # A single deadline also bounds unexpected notification floods.
                deadline = time.monotonic() + 60
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError(f"ACP {method} did not respond within 60 seconds")
                    line = messages.get(timeout=remaining)
                    assert line is not None, "ACP stdout closed before response"
                    response = json.loads(line)
                    if response.get("id") == request_id:
                        assert response.get("jsonrpc") == "2.0", response
                        return response

            try:
                response = rpc(1, "initialize", {"protocolVersion": 1})
                assert "result" in response, response
                assert response["result"]["protocolVersion"] == 1, response
                assert isinstance(response["result"]["agentCapabilities"], dict), response
                response = rpc(2, "session/new", {"cwd": str(fixture), "mcpServers": []})
                assert "result" in response, response
                session_id = response["result"]["sessionId"]
                assert isinstance(session_id, str) and session_id, response
                response = rpc(3, "loongarch64-smoke/unknown", {})
                assert response["error"]["code"] == -32601, response
                process.stdin.close()
                assert process.wait(timeout=30) == 0, "ACP failed on EOF shutdown"
                reader.join(timeout=5)
                assert (fixture / "threads.db").stat().st_size > 0, "SQLite database missing"
                print("PASS: ACP initialize, session/new, method-not-found, SQLite, EOF shutdown")
            except BaseException:
                stderr.flush()
                stderr.seek(0)
                print(stderr.read())
                raise
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
                process.stdout.close()
        result = subprocess.run(
            command + ["--db-path", str(fixture / "threads.db"),
                       "meta", "session", session_id, "--json"],
            input="", text=True, capture_output=True, timeout=60, env=environment,
        )
        assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
        metadata = json.loads(result.stdout)
        assert metadata["id"] == session_id, metadata
        assert metadata["cwd"] == str(fixture), metadata
        print("PASS: persisted session metadata reopened by a new process")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", type=Path,
                        default=Path(__file__).resolve().parent.parent /
                        "target/loongarch64-unknown-linux-musl/release/peri")
    parser.add_argument("--qemu", default=QEMU)
    args = parser.parse_args()
    if os.environ.get("PYTHONOPTIMIZE"):
        parser.error("Run without PYTHONOPTIMIZE so validation assertions remain enabled")
    if not __debug__:
        parser.error("Run without -O so validation assertions remain enabled")
    check_elf(args.binary.resolve())
    check_runtime(args.binary.resolve(), args.qemu)


if __name__ == "__main__":
    main()
