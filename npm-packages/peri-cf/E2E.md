# Live browser E2E

Operational guide, not an architectural contract. See [module guidance](CLAUDE.md),
[testing standard](../../docs/standards/testing.md), and
[secret handling](../../docs/standards/architecture-contracts.md#arc-secret-001).

## Prerequisites and execution

The runner also queries `/api/chats/:id/resources` after completed turns and
during streaming/stopping. It checks real exported WASM memory, distinct Host
identities across turns, live versus final observations, and explicitly unavailable
instance CPU values. These checks do not treat wall-clock prompt latency as CPU time.

Start the application with the real server configuration and wait until it is
ready. This runner does **not** start, build, configure, or reset the server.
From `npm-packages/peri-cf`, then run:

```sh
bun run test:e2e
# Optional visible browser and slower provider allowance:
BASE_URL=http://127.0.0.1:8791 E2E_HEADED=1 E2E_REPLY_TIMEOUT_MS=600000 bun run test:e2e
```

Node must be available. The runner uses an installed `playwright` / `@playwright/test`
or an existing npm `_npx` cached Playwright with installed Chromium. It neither
downloads dependencies nor launches a fixture server. To use a specific installation,
set `E2E_PLAYWRIGHT_MODULE` to its absolute `playwright/index.mjs` path. If none is
available, provision Playwright and its Chromium browser separately before running.

The token is read from the server's package-local `.dev.vars` `APP_AUTH_TOKEN`;
`E2E_DEV_VARS` may specify another server vars file (relative to this package, or absolute).
An environment `APP_AUTH_TOKEN` is intentionally not used. No token is passed on
the command line, injected into storage, or placed in a URL: login uses the settings UI.
Do not enable external browser/network logging. The runner disables Playwright
debug logging and emits only fixed stage/assertion codes; it does not emit raw
errors, console messages, URLs, HTTP bodies, WS payloads, traces, screenshots,
videos, or storage state. Token input remains masked.

## Coverage and effects

- Starts an isolated browser context; logs in via **我的空间 → 保存设置**.
- Creates a uniquely tagged real conversation through the composer and checks an
  actual assistant answer, then a second answer remembering the first-turn identifier.
- Observes real `/api/chats/.../sync` snapshot/update frames without altering APIs,
  model traffic, Yjs, or WebSocket behavior.
- Reloads, reopens saved history, leaves/reselects it, and compares exact rendered replies.
- Requests a long real story, waits for actual streamed text, clicks **停止生成**,
  waits for the persisted stopped state, then sends a short continuation.
- Reloads again to verify all four turns, partial text, and cancellation status.
- At a 390×844 mobile viewport, checks document overflow with history, sidebar,
  and a populated composer; navigation remains UI-driven.
- Fails on any observed HTTP response ≥500, authentication rejection, pageerror,
  malformed sync frame, WS transport error, missing live sync, or final UI error.

This accesses the configured real model and storage, may incur charges, and leaves
one tagged conversation per invocation. It never deletes existing history. A failed
run may leave a generation running; inspect the application and stop it through the UI.
Run serially without another user updating the tested chat in this shared workspace.
History selection uses the actual selected chat identity rather than its mutable
title, and verifies the unique first prompt and exact reply contents.
No retry/replay is automatic. If the long response finishes before Stop can be clicked,
the cancellation check fails rather than claiming it was tested.

## Wait controls

All duration values are positive integer milliseconds:

| Variable | Default | Purpose |
| --- | --- | --- |
| `BASE_URL` | `http://127.0.0.1:8791` | Existing application's HTTP origin |
| `E2E_ACTION_TIMEOUT_MS` | `30000` | Individual UI interactions |
| `E2E_READY_TIMEOUT_MS` | `180000` | Navigation, history, synchronization |
| `E2E_REPLY_TIMEOUT_MS` | `300000` | Each real model answer / first long-answer text |
| `E2E_STOP_TIMEOUT_MS` | `120000` | Real cancellation confirmation |
| `E2E_SLOW_MO_MS` | `50` | Browser action pacing |
| `E2E_HEADED` | unset | Set `1` to watch the browser |

Exit status is nonzero on failure. A PASS covers only that run and configured live
environment; it is not evidence for startup recovery or production deployment.

Observed results and unresolved failures are recorded in [E2E-RESULTS.md](E2E-RESULTS.md).
