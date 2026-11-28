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
