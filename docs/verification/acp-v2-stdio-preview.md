# ACP v2 stdio Preview verification

Base: `4b0da81f7c82d4ee12e4a5cc4ba99b849f8d9e3e`. Repository toolchain Rust
1.99.0, Linux x86_64. All runs use an isolated SQLite database and a
loopback-only deterministic OpenAI-compatible SSE model fixture.

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri
python3 scripts/verification/acp_v2_stdio.py target/debug/peri \
  /path/to/verified-acp-checkout/schema/v2/schema.json \
  --official-client /path/to/v2_one_shot_client
```

Use ACP schema commit `927e31867b31b1e3c961c7edee7e3680b90be718`. The client
needs Python with `jsonschema` and Node.js. It launches the shipped Peri CLI
entry point via stdio. It does not import Peri's server or mocks for JSON-RPC.
The independently installed ACP Rust SDK example is an additional client
check, not a substitute for the matrix.

Release gate: the build, Rust protocol-unit checks, SDK example, and raw wire
matrix must all pass on the same patch. The matrix covers paired v1/v2
negotiation, acceptance and stable IDs, retry without duplicate insertion,
streaming, process restart and replay, request and session cancellation,
denied approval with no file side effect, and close. This does not imply
complete conformance for optional v2 facilities, real provider load tests,
cross-platform execution, or support for other transports.

## Review follow-up on `acdd851df`

Re-verified the review fixes with the same command on macOS arm64, isolated
SQLite database and loopback model. The matrix additionally asserts:

- repeated `initialize` before the first other request succeeds, a failed
  repeat keeps the last successful negotiation, a later repeat is `-32600`, and
  `protocolVersion: 65536` is `-32602`;
- the adapter's one-active-session limit (`session/new` and a resume of a
  different session are `-32000` while a session is active);
- idempotency keys on the wire: 256 characters rejected, a 255-character key
  accepted and deduplicated;
- the approval probe starts with an explicit `--permission-mode bypass`, so its
  permission request and absent tool side effect prove the negotiated v2
  connection overrides bypass with the default approval policy.

The probe drains stderr while the agent runs and reaps the child when
`initialize` fails. All five original PASS lines and the added protocol-edge
line passed; the official `v2_one_shot_client` example passed in the same run.

## S1 multi-session and explicit negotiation

Base: `0b6ca2f74`. Same command, isolated SQLite database, loopback model,
macOS arm64; `--official-client` used the unmodified 3.5.0 `v2_one_shot_client`.
The matrix and the official client both exited 0.

This run **supersedes** the earlier one-active-session assertion above: a v2
connection now holds several sessions, so `session/new` while another session is
attached succeeds and a resume of a session the connection never attached fails
with the host's code (never `-32000`). The matrix now asserts:

- two sessions on one connection, a turn on the second one, then **two turns in
  flight at the same time** on the two sessions, then a single close — the
  closed session's prompt is `-32002` while the other still runs;
- `session/delete` removes the session from `session/list` and from the
  connection (`session/prompt` and a second `session/delete` are `-32002`);
- error-code shapes: unknown session `-32002` with `data.sessionId`, missing and
  relative `cwd` `-32602`, unadvertised `additionalDirectories` `-32602`,
  `terminal/cancel` and `tool_call/cancel` `-32602`, `auth/login` `-32601`;
- the previous checks (repeat `initialize`, key bounds and dedup, replay
  ordering, both cancellation scopes, durable withdrawal, approval probe, v1
  client with the preview on and off) are unchanged.

Adapter-level behaviour that has no wire probe is covered by
`cargo test -p peri-acp --lib -- host::stdio::v2`: per-session routing and close
isolation, stale-epoch completions, concurrent permission aggregation,
`Attaching` hold/flush ordering, elicitation capability gating, `$/cancel_request`
scope, and delete settlement.

## S2 durable idempotency keys

Base: `e355ae1dc`. Same command, isolated SQLite database, loopback model,
macOS arm64; `--official-client` used the unmodified 3.5.0 `v2_one_shot_client`.
The matrix and the official client both exited 0.

A keyed prompt now derives its message id from `(sessionId, key)`
(`UUIDv5` with a frozen namespace), so the durable store answers a retry and no
connection-local receipt survives a restart. The matrix adds one PASS line that
spans three processes on the same database and `cwd`:

- process A prompts with a 203-character key and exits; process B resumes,
  repeats the identical submission, and gets the **same** `messageId` with no
  `user_message` update for it (the retry is acknowledged from the store);
- the same key with edited content is `-32602`, and the same key in a second
  session yields a different message id;
- a third process replays from the start of history: the retried submission
  appears exactly once as a `user_message` boundary.

Adapter and mailbox behaviour behind that line is covered by unit tests:
`cargo test -p peri-acp --lib -- host::stdio::v2` (derived-id stability and
version, an in-flight retry joining the first submission, a retry after the
acknowledgement reusing the identity, `delivered` acknowledgement releasing the
waiters, keys scoped per session, `-32602` for different content while in
flight), `cargo test -p peri-agent --lib -- user_input_mailbox` (a restarted
mailbox over the same store reports `Delivered` for an already-handed-off id,
`Inserted`/`Existing` stay `Queued`, and different content under that id is a
conflict), and `cargo test -p peri-resources --lib -- session_mq` (SQLite and,
with `PERI_TEST_POSTGRES_URL`, the remote adapter return `AlreadyHandedOff` for
the same payload and a conflict error for a different one).

Known limits of this stage: the second connection of a *live* process that
attaches the same session does not receive the first connection's delivery
event, so its same-key retry waits for `close`/`cancel` instead of being
acknowledged (no duplicate message is inserted); a key retired by a confirmed
withdrawal is answered with `-32800` until the session generation restarts;
`session/list` pagination, replay cursors and the config surface remain out of
scope (S3/S4).
