#!/usr/bin/env python3
"""Independent ACP v2 stdio wire client with schema checks and a loopback model.

Usage: python3 scripts/verification/acp_v2_stdio.py target/debug/peri PATH/schema/v2/schema.json
Requires Python jsonschema and Node.js. Does not call an external model.
"""

import argparse
import collections
import contextlib
import json
import os
from pathlib import Path
import queue
import shlex
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

from jsonschema import Draft202012Validator


class Peer:
    def __init__(self, binary, root, version, schema, preview=True, permission="bypass"):
        self.version = version
        self.schema = schema
        self.seq = 0
        self.sid = None
        self.frames = []
        self.mail = queue.Queue()
        self.stderr_tail = collections.deque(maxlen=200)
        env = dict(os.environ, HOME=str(root / "home"))
        env.pop("PERI_ACP_V2_PREVIEW", None)
        if preview:
            env["PERI_ACP_V2_PREVIEW"] = "1"
        self.agent = subprocess.Popen(
            [str(binary), "--config-file", str(root / "settings.json"),
             "--db-path", str(root / "sessions.sqlite"), "--permission-mode", permission,
             "acp", "--cwd", str(root)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, env=env, cwd=root,
        )
        threading.Thread(target=self.read, daemon=True).start()
        # Drain stderr while the agent runs; a full pipe would otherwise block it.
        threading.Thread(target=self.read_stderr, daemon=True).start()
        init = {"protocolVersion": version}
        if version == 2:
            init["info"] = {"name": "peri-independent-stdio-probe", "version": "1"}
        try:
            assert self.call("initialize", init, "InitializeResponse")["protocolVersion"] == (
                2 if preview and version == 2 else 1
            )
        except BaseException:
            # `lifecycle` only tracks successfully constructed peers; reap the
            # child here so a silent initialize keeps no orphan.
            if self.agent.poll() is None:
                self.agent.kill()
            self.agent.wait()
            raise

    @property
    def stderr_text(self):
        return "".join(self.stderr_tail)[-2000:]

    def read(self):
        for line in self.agent.stdout:
            self.mail.put(line)
        self.mail.put(None)

    def read_stderr(self):
        for line in self.agent.stderr:
            self.stderr_tail.append(line)

    def check(self, definition, obj):
        if self.version == 2 and definition:
            definitions = self.schema["$defs"]
            assert definition in definitions, f"Missing ACP schema definition: {definition}"
            errors = Draft202012Validator({
                "$defs": definitions, "$ref": "#/$defs/" + definition
            }).iter_errors(obj)
            first = next(errors, None)
            assert first is None, f"{definition}: {first.message if first else ''}"

    def send(self, payload):
        self.agent.stdin.write(json.dumps({"jsonrpc": "2.0", **payload}) + "\n")
        self.agent.stdin.flush()

    def request(self, method, params):
        self.seq += 1
        self.send({"id": self.seq, "method": method, "params": params})
        return self.seq

    def until(self, predicate, timeout=25):
        due = time.monotonic() + timeout
        for frame in self.frames:
            if predicate(frame):
                return frame
        while time.monotonic() < due:
            try:
                line = self.mail.get(timeout=max(0, due - time.monotonic()))
            except queue.Empty:
                break
            if line is None:
                raise AssertionError("Agent EOF: " + self.stderr_text)
            frame = json.loads(line)
            self.frames.append(frame)
            if frame.get("method") == "session/update":
                self.check("UpdateSessionNotification", frame["params"])
            if predicate(frame):
                return frame
        raise AssertionError(f"No matching frame; most recent frames: {self.frames[-12:]}")

    def response(self, rid, definition=None):
        frame = self.until(lambda f: f.get("id") == rid and "method" not in f)
        if "result" in frame:
            self.check(definition, frame["result"])
        return frame

    def call(self, method, params, definition=None):
        frame = self.response(self.request(method, params), definition)
        assert "result" in frame, frame
        return frame["result"]

    def update(self, after, name, **expected):
        return self.until(lambda f: f in self.frames[after:]
                          and f.get("method") == "session/update"
                          and f["params"]["sessionId"] == self.sid
                          and f["params"]["update"].get("sessionUpdate") == name
                          and all(f["params"]["update"].get(k) == v for k, v in expected.items()))

    def idle(self, after, reason="end_turn"):
        return self.update(after, "state_update", state="idle", stopReason=reason)

    def close(self):
        if self.agent.poll() is None:
            self.agent.stdin.close()
            try:
                self.agent.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.agent.kill()
                self.agent.wait()
                raise
        assert self.agent.returncode == 0, self.stderr_text


def prompt(sid, text, key=None):
    result = {"sessionId": sid, "prompt": [{"type": "text", "text": text}]}
    if key:
        result["idempotencyKey"] = key
    return result


def lifecycle(binary, root, schema):
    peers = []
    try:
        peer = Peer(binary, root, 2, schema)
        peers.append(peer)
        # Repeating initialize is allowed before the first other request.
        # A failed repeat leaves the last successful negotiation in effect.
        client = {"name": "peri-independent-stdio-probe", "version": "1"}
        assert peer.call("initialize", {"protocolVersion": 2, "info": client},
                         "InitializeResponse")["protocolVersion"] == 2
        assert peer.response(peer.request("initialize", {"protocolVersion": 2}))["error"]["code"] == -32602
        assert peer.response(peer.request("initialize", {"protocolVersion": 65536, "info": client}))["error"]["code"] == -32602
        sid = peer.sid = peer.call("session/new", {"cwd": str(root)}, "NewSessionResponse")["sessionId"]
        assert peer.response(peer.request("initialize", {"protocolVersion": 2, "info": client}))["error"]["code"] == -32600
        # v2 addresses every session by id, so one connection may hold several.
        other = peer.call("session/new", {"cwd": str(root)}, "NewSessionResponse")["sessionId"]
        assert other != sid
        # A resume of a session this connection never attached is a host-level
        # failure, never the v1-style "one active session" rejection: `-32000`
        # is reserved for Authentication required in the pinned schema.
        unknown = peer.response(peer.request("session/resume", {
            "sessionId": "00000000-0000-7000-8000-000000000000", "cwd": str(root)
        }))["error"]
        assert unknown["code"] != -32000, unknown
        # cwd is a required absolute path; the preview advertises neither extra
        # roots nor system prompts.
        assert peer.response(peer.request("session/new", {}))["error"]["code"] == -32602
        assert peer.response(peer.request("session/new", {"cwd": "relative"}))["error"]["code"] == -32602
        assert peer.response(peer.request("session/new", {
            "cwd": str(root), "additionalDirectories": [str(root)]
        }))["error"]["code"] == -32602
        # Per-object cancellation is never advertised.
        assert peer.response(peer.request("terminal/cancel", {
            "sessionId": sid, "terminalId": "t"
        }))["error"]["code"] == -32602
        assert peer.response(peer.request("tool_call/cancel", {
            "sessionId": sid, "toolCallId": "t"
        }))["error"]["code"] == -32602
        assert peer.response(peer.request("auth/login", {}))["error"]["code"] == -32601
        # An unknown session names itself in the error data.
        ghost = "00000000-0000-7000-8000-000000000001"
        missing = peer.response(peer.request("session/prompt", prompt(ghost, "ghost")))["error"]
        assert missing["code"] == -32002 and missing["data"]["sessionId"] == ghost, missing
        # Idempotency keys are 1..255 characters, not bytes.
        assert peer.response(peer.request("session/prompt", prompt(sid, "Too long", "k" * 256)))["error"]["code"] == -32602
        print("PASS v2 initialize repeat/version bounds, multi-session admission, "
              "setup/error-code shapes, prompt key bounds")
        # Cross-session routing: a turn on the second session stays on it.
        start = len(peer.frames)
        assert "result" in peer.response(peer.request("session/prompt", prompt(other, "Second session turn")),
                                        "PromptResponse")
        peer.until(lambda f: f in peer.frames[start:]
                   and f.get("method") == "session/update"
                   and f["params"]["sessionId"] == other
                   and f["params"]["update"].get("sessionUpdate") == "state_update"
                   and f["params"]["update"].get("state") == "idle")
        # Turns of two sessions may be in flight at the same time: the host
        # serializes per session, not per connection.
        start = len(peer.frames)
        first = peer.request("session/prompt", prompt(sid, "Concurrent turn A"))
        second = peer.request("session/prompt", prompt(other, "Concurrent turn B"))
        peer.response(first, "PromptResponse")
        peer.response(second, "PromptResponse")
        for target in (sid, other):
            peer.until(lambda f: f in peer.frames[start:]
                       and f.get("method") == "session/update"
                       and f["params"]["sessionId"] == target
                       and f["params"]["update"].get("sessionUpdate") == "state_update"
                       and f["params"]["update"].get("state") == "idle")
        peer.call("session/close", {"sessionId": other}, "CloseSessionResponse")
        assert peer.response(peer.request("session/prompt", prompt(other, "Closed")))["error"]["code"] == -32002
        print("PASS v2 two sessions on one connection; concurrent turns; per-session close isolation")
        # session/delete removes the session from the list and its slot.
        doomed = peer.call("session/new", {"cwd": str(root)}, "NewSessionResponse")["sessionId"]
        peer.call("session/delete", {"sessionId": doomed}, "DeleteSessionResponse")
        assert peer.response(peer.request("session/prompt", prompt(doomed, "gone")))["error"]["code"] == -32002
        assert all(x["sessionId"] != doomed for x in
                   peer.call("session/list", {}, "ListSessionsResponse")["sessions"])
        assert peer.response(peer.request("session/delete", {"sessionId": doomed}))["error"]["code"] == -32002
        print("PASS v2 session/delete drops the session and its connection slot")
        # A 255-character key is the accepted upper bound; the 256-character
        # key above was rejected without creating a receipt.
        key = "k" * 255
        args = prompt(sid, "Please reply to this ACP Preview verification prompt.", key)
        start = len(peer.frames)
        request = peer.request("session/prompt", args)
        duplicate = peer.request("session/prompt", args)
        receipt = peer.response(request, "PromptResponse")
        message_id = receipt["result"]["messageId"]
        assert peer.response(duplicate, "PromptResponse")["result"]["messageId"] == message_id
        peer.update(start, "user_message", messageId=message_id)
        peer.idle(start)
        assert any(x["sessionId"] == sid for x in peer.call("session/list", {}, "ListSessionsResponse")["sessions"])
        emitted = [frame["params"]["update"] for frame in peer.frames[start:] if
                   frame.get("method") == "session/update"]
        assert sum(update.get("messageId") == message_id for update in emitted) == 1
        assert any(update["sessionUpdate"] == "agent_message_chunk" for update in emitted)
        assert peer.response(peer.request("session/prompt", prompt(sid, "Changed", key)))["error"]["code"] == -32602
        assert peer.frames.index(receipt) < next(i for i in range(start, len(peer.frames)) if
               peer.frames[i].get("params", {}).get("update", {}).get("sessionUpdate") == "agent_message_chunk")
        print("PASS v2 negotiate/new/list; committed echo + early ACK; streamed answer, idle, keyed dedup/conflict")
        peer.call("session/close", {"sessionId": sid}, "CloseSessionResponse")
        assert peer.response(peer.request("session/prompt", prompt(sid, "Closed")))["error"]["code"] == -32002
        peer.close()

        restored = Peer(binary, root, 2, schema)
        peers.append(restored)
        restored.sid = sid
        assert restored.response(restored.request("session/resume", {
            "sessionId": sid, "cwd": str(root), "replayFrom": {"type": "unsupported"}
        }))["error"]["code"] == -32602
        replay_at = len(restored.frames)
        resume = restored.request("session/resume", {"sessionId": sid, "cwd": str(root), "replayFrom": {"type": "start"}})
        response = restored.response(resume, "ResumeSessionResponse")
        assert "result" in response, response
        updates = [f["params"]["update"] for f in restored.frames[replay_at:restored.frames.index(response)]
                   if f.get("method") == "session/update"]
        assert sum(u["sessionUpdate"] == "replay_complete" for u in updates) == 1
        boundary = next(i for i, u in enumerate(updates) if u["sessionUpdate"] == "replay_complete")
        assert any(u.get("messageId") == message_id for u in updates[:boundary])
        assert updates[boundary + 1]["state"] == "idle"
        start = len(restored.frames)
        request = restored.request("session/prompt", prompt(sid, "A second turn on the restored session"))
        restored.response(request, "PromptResponse")
        restored.idle(start)
        start = len(restored.frames)
        request = restored.request("session/prompt", prompt(sid, "Cancel this streaming turn after insertion"))
        receipt = restored.response(request, "PromptResponse")
        assert "messageId" in receipt["result"]
        pending = restored.request("session/prompt", prompt(sid, "This queued prompt must not run"))
        restored.send({"method": "$/cancel_request", "params": {"requestId": pending}})
        assert restored.response(pending)["error"]["code"] == -32800
        restored.send({"method": "session/cancel", "params": {"sessionId": sid}})
        restored.idle(start, "cancelled")
        restored.call("session/close", {"sessionId": sid}, "CloseSessionResponse")
        restored.call("session/close", {"sessionId": sid}, "CloseSessionResponse")
        restored.close()
        reopened = Peer(binary, root, 2, schema)
        peers.append(reopened)
        reopened.sid = sid
        reopened.call("session/resume", {"sessionId": sid, "cwd": str(root),
                                        "replayFrom": {"type": "start"}}, "ResumeSessionResponse")
        assert "This queued prompt must not run" not in json.dumps(reopened.frames)
        reopened.call("session/close", {"sessionId": sid}, "CloseSessionResponse")
        reopened.close()
        print("PASS v2 reopen same DB/root; replay and state ordering; fresh turn; both cancellation scopes; durable withdrawal; idempotent close")

        # Explicit bypass must be overridden by the negotiated v2 connection;
        # otherwise this probe would see the tool auto-approved and fail.
        approval = Peer(binary, root, 2, schema, permission="bypass")
        peers.append(approval)
        approval.sid = approval.call("session/new", {"cwd": str(root)}, "NewSessionResponse")["sessionId"]
        start = len(approval.frames)
        request = approval.request("session/prompt", prompt(approval.sid, "[[tool:verify]] approval probe"))
        approval.response(request, "PromptResponse")
        permission = approval.until(lambda f: f.get("method") == "session/request_permission")
        approval.check("RequestPermissionRequest", permission["params"])
        approval.update(start, "state_update", state="requires_action")
        options = permission["params"]["options"]
        reject = next(o["optionId"] for o in options if o["kind"] == "reject_once")
        approval.send({"id": permission["id"], "result": {"outcome": {"outcome": "selected", "optionId": reject}}})
        approval.idle(start)
        assert not (root / "approval-probe.txt").exists()
        start = len(approval.frames)
        request = approval.request("session/prompt", prompt(approval.sid, "[[tool:verify]] cancel approval"))
        approval.response(request, "PromptResponse")
        permission = approval.until(lambda f: f in approval.frames[start:]
                                    and f.get("method") == "session/request_permission")
        approval.send({"method": "session/cancel", "params": {"sessionId": approval.sid}})
        cancelled = approval.until(lambda f: f.get("method") == "$/cancel_request"
                                   and f.get("params", {}).get("requestId") == permission["id"])
        assert cancelled["params"]["requestId"] == permission["id"]
        approval.idle(start, "cancelled")
        assert not (root / "approval-probe.txt").exists()
        approval.call("session/close", {"sessionId": approval.sid}, "CloseSessionResponse")
        approval.close()
        print("PASS v2 real permission request, requires_action, rejection, peer cancellation, idle, no tool side effect")

        for enabled in [True, False]:
            legacy = Peer(binary, root, 1, schema, preview=enabled)
            peers.append(legacy)
            legacy.sid = legacy.call("session/new", {"cwd": str(root), "mcpServers": []})["sessionId"]
            result = legacy.call("session/prompt", prompt(legacy.sid, "Legacy client still works"))
            assert result["stopReason"] == "end_turn", result
            legacy.call("session/close", {"sessionId": legacy.sid})
            legacy.close()
        print("PASS v1 client with Preview enabled and disabled")
    finally:
        for peer in peers:
            if peer.agent.poll() is None:
                peer.agent.kill()
                peer.agent.wait()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("schema", type=Path)
    parser.add_argument("--official-client", type=Path,
                        help="Built, unmodified upstream v2_one_shot_client")
    args = parser.parse_args()
    schema = json.loads(args.schema.read_text())
    with tempfile.TemporaryDirectory(prefix="peri-acp-preview-") as temp, contextlib.ExitStack() as stack:
        root = Path(temp).resolve()
        (root / "home").mkdir()
        with socket.socket() as socket_probe:
            socket_probe.bind(("127.0.0.1", 0))
            port = socket_probe.getsockname()[1]
        stub = subprocess.Popen(
            ["node", str(Path(__file__).resolve().parents[2] / "peri-swarm/scripts/local_model_stub.mjs")],
            env={**os.environ, "PERI_STUB_PORT": str(port), "PERI_STUB_CHUNK_DELAY_MS": "135",
                 "PERI_STUB_COMMANDS": json.dumps({"verify": "printf probe > approval-probe.txt"})},
            stdout=subprocess.DEVNULL,
        )
        stack.callback(stub.wait)
        stack.callback(stub.terminate)
        for _ in range(100):
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1) as response:
                    assert response.status == 200
                break
            except OSError:
                time.sleep(0.05)
        else:
            raise RuntimeError("Loopback model failed to start")
        (root / "settings.json").write_text(json.dumps({"config": {
            "active_alias": "sonnet",
            "providers": [{"id": "stub", "type": "openai", "apiKey": "not-a-secret",
                           "baseUrl": f"http://127.0.0.1:{port}/v1"}],
            "profiles": {"sonnet": {"provider": "stub", "model": "stub-model"}}
        }}))
        lifecycle(args.binary.resolve(), root, schema)
        if args.official_client:
            launcher = root / "launch-peri"
            command = [str(args.binary.resolve()), "--config-file", str(root / "settings.json"),
                       "--db-path", str(root / "sdk-sessions.sqlite"), "acp", "--cwd", str(root)]
            launcher.write_text("#!/bin/sh\nexec " + shlex.join(command) + "\n")
            launcher.chmod(0o700)
            probe = subprocess.run(
                [str(args.official_client.resolve()), "--command", str(launcher), "Verify ACP v2"],
                capture_output=True, text=True, cwd=root, timeout=30,
                env={**os.environ, "HOME": str(root / "home"), "PERI_ACP_V2_PREVIEW": "1"},
            )
            assert probe.returncode == 0, probe.stderr
            assert "stub-echo:" in probe.stdout and "Some(EndTurn)" in probe.stderr
            print("PASS upstream Rust ACP 3.5.0 v2_one_shot_client: accepted, streamed, idle, closed")


if __name__ == "__main__":
    main()
