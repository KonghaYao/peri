#!/usr/bin/env python3
"""Check the ELF contract and exercise the built binary in 32-bit Linux Docker."""

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
import uuid


def check_elf(binary):
    data = binary.read_bytes()
    assert data[:7] == b"\x7fELF\x01\x01\x01", "Expected little-endian ELF32"
    header = struct.unpack_from("<HHIIIIIHHHHHH", data, 16)
    assert header[0] == 2 and header[1] == 3, "Expected Intel 80386 ET_EXEC"
    offset, entry_size, count = header[4], header[8], header[9]
    assert entry_size == 32 and count > 0, "Invalid ELF program headers"
    types = [struct.unpack_from("<I", data, offset + i * entry_size)[0]
             for i in range(count)]
    assert 1 in types, "Missing LOAD segment"
    assert 2 not in types and 3 not in types, "Dynamic segment or ELF interpreter found"
    print("PASS: ELF32 Intel 80386, static executable, no interpreter/dynamic segment")


def check_runtime(binary, image):
    with tempfile.TemporaryDirectory(prefix="peri-i386-") as temporary:
        fixture = Path(temporary)
        (fixture / "home").mkdir()
        (fixture / "settings.json").write_text(json.dumps({"config": {
            "active_alias": "sonnet",
            "providers": [{"id": "smoke", "type": "openai",
                           "apiKey": "not-a-credential"}],
            "profiles": {"sonnet": {"provider": "smoke", "model": "test-model"}},
        }}))
        name = "peri-i386-smoke-" + uuid.uuid4().hex
        command = [
            "docker", "run", "--rm", "--name", name, "--network", "none",
            "--platform", "linux/386", "-i",
            "--mount", f"type=bind,src={binary},dst=/peri,readonly",
            "--mount", f"type=bind,src={fixture},dst=/smoke",
            "--env", "HOME=/smoke/home", "--workdir", "/smoke", image, "/peri",
        ]
        try:
            for args, code, marker in [
                (["--version"], 0, "peri "),
                (["--help"], 0, "Usage:"),
                (["--invalid-i386-smoke-option"], 2, "unexpected argument"),
            ]:
                result = subprocess.run(command + args, input="", text=True,
                                        capture_output=True, timeout=60)
                output = result.stdout + result.stderr
                assert result.returncode == code, (args, result.returncode, output)
                assert marker in output, (args, output)
                print(f"PASS: {' '.join(args)} (exit {code})")

            with (fixture / "stderr.log").open("w+") as stderr:
                process = subprocess.Popen(
                    command + ["--config-file", "/smoke/settings.json", "--db-path",
                               "/smoke/threads.db", "acp", "--cwd", "/smoke"],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                    text=True,
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
                    response = rpc(2, "session/new", {"cwd": "/smoke", "mcpServers": []})
                    assert "result" in response, response
                    session_id = response["result"]["sessionId"]
                    assert isinstance(session_id, str) and session_id, response
                    response = rpc(3, "i386-smoke/unknown", {})
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
                        subprocess.run(["docker", "rm", "-f", name], capture_output=True, timeout=30)
                        process.kill()
                        process.wait(timeout=10)
                    process.stdout.close()
            result = subprocess.run(
                command + ["--db-path", "/smoke/threads.db", "meta", "session", session_id, "--json"],
                input="", text=True, capture_output=True, timeout=60,
            )
            assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
            metadata = json.loads(result.stdout)
            assert metadata["id"] == session_id and metadata["cwd"] == "/smoke", metadata
            print("PASS: persisted session metadata reopened by a new process")
        finally:
            subprocess.run(["docker", "rm", "-f", name], capture_output=True, timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", type=Path,
                        default=Path(__file__).resolve().parent.parent /
                        "target/i686-unknown-linux-musl/release/peri")
    parser.add_argument("--image", default="peri-i386-smoke:local")
    args = parser.parse_args()
    if os.environ.get("PYTHONOPTIMIZE"):
        parser.error("Run without PYTHONOPTIMIZE so validation assertions remain enabled")
    if not __debug__:
        parser.error("Run without -O so validation assertions remain enabled")
    check_elf(args.binary.resolve())
    check_runtime(args.binary.resolve(), args.image)


if __name__ == "__main__":
    main()
