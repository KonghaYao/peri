#!/usr/bin/env python3
"""Create the Rivet pool that peri-swarm registers its actor into.

Rivet's control plane refuses a node whose pool does not exist, and the gateway
runs with PERI_SWARM_RIVET_ENGINE_SPAWN=never, so no other component creates the
pool. This one-shot container waits for the engine, then issues one PUT.

The operation is idempotent, so re-running it against a live stack is harmless.
Exit code 0 means the pool exists; 1 means the engine or its API did not become
ready in time.
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request

ENGINE_URL = os.environ.get("RIVET_ENGINE_URL", "http://rivet-engine:6420").rstrip("/")
ADMIN_TOKEN = os.environ.get("RIVET_ADMIN_TOKEN", "")
POOL = os.environ.get("RIVET_POOL", "peri-swarm")
NAMESPACE = os.environ.get("RIVET_NAMESPACE", "default")
HEALTH_TIMEOUT = float(os.environ.get("RIVET_HEALTH_TIMEOUT_S", "180"))
POOL_TIMEOUT = float(os.environ.get("RIVET_POOL_TIMEOUT_S", "180"))
POLL_INTERVAL = float(os.environ.get("RIVET_POLL_INTERVAL_S", "2"))
REQUEST_TIMEOUT = float(os.environ.get("RIVET_REQUEST_TIMEOUT_S", "10"))


def log(message: str) -> None:
    print(f"rivet-init: {message}", flush=True)


def call(method: str, path: str, payload: bytes | None = None) -> tuple[int, str]:
    """Return (status, body). HTTP errors keep their body for diagnosis."""
    request = urllib.request.Request(
        f"{ENGINE_URL}{path}",
        data=payload,
        method=method,
        headers={
            "Authorization": f"Bearer {ADMIN_TOKEN}",
            "Content-Type": "application/json",
            "Accept": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode("utf-8", "replace")


def wait_for_health() -> None:
    deadline = time.monotonic() + HEALTH_TIMEOUT
    last = "no attempt completed"
    while time.monotonic() < deadline:
        try:
            status, body = call("GET", "/health")
        except OSError as error:
            last = f"{type(error).__name__}: {error}"
        else:
            if status == 200:
                try:
                    payload = json.loads(body)
                except json.JSONDecodeError:
                    last = f"HTTP 200 with a non-JSON body ({body[:120]!r})"
                else:
                    if payload.get("status") == "ok":
                        log(f"engine healthy at {ENGINE_URL}")
                        return
                    last = f"HTTP 200 with status={payload.get('status')!r}"
            else:
                last = f"HTTP {status} from /health"
        time.sleep(POLL_INTERVAL)
    raise SystemExit(
        f"rivet-init: engine did not become healthy within {HEALTH_TIMEOUT:.0f}s ({last})"
    )


def pool_exists() -> tuple[bool, str]:
    try:
        status, _ = call("GET", f"/pools/{POOL}/nodes?namespace={NAMESPACE}")
    except OSError as error:
        return False, f"{type(error).__name__}: {error}"
    return status == 200, f"HTTP {status}"


def ensure_pool() -> None:
    path = f"/pools/{POOL}?namespace={NAMESPACE}"
    deadline = time.monotonic() + POOL_TIMEOUT
    last = "no attempt completed"
    while time.monotonic() < deadline:
        try:
            status, body = call("PUT", path, b"{}")
        except OSError as error:
            last = f"{type(error).__name__}: {error}"
        else:
            if 200 <= status < 300:
                log(f"pool {POOL!r} ready (namespace {NAMESPACE!r}, HTTP {status})")
                return
            if status in (401, 403):
                raise SystemExit(
                    "rivet-init: engine rejected RIVET_ADMIN_TOKEN "
                    f"(HTTP {status}): {body[:200]}"
                )
            if status == 409 or "already" in body.lower():
                log(f"pool {POOL!r} already present (HTTP {status})")
                return
            last = f"HTTP {status}: {body[:200]}"
            # `/health` answers before storage is ready, so `depot.overloaded`
            # right after startup is expected: just retry the PUT. For other
            # errors a pool that already exists is still success, so confirm
            # before treating it as fatal.
            if "overloaded" not in body.lower():
                exists, probe = pool_exists()
                if exists:
                    log(f"pool {POOL!r} already present (confirmed by nodes probe)")
                    return
                last = f"{last}; nodes probe: {probe}"
        time.sleep(POLL_INTERVAL)
    raise SystemExit(
        f"rivet-init: pool {POOL!r} not ready within {POOL_TIMEOUT:.0f}s ({last})"
    )


def main() -> int:
    if not ADMIN_TOKEN.strip():
        raise SystemExit("rivet-init: RIVET_ADMIN_TOKEN is empty")
    for name, value in (("RIVET_POOL", POOL), ("RIVET_NAMESPACE", NAMESPACE)):
        if not value.strip() or "/" in value:
            raise SystemExit(f"rivet-init: invalid {name} {value!r}")
    wait_for_health()
    ensure_pool()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
