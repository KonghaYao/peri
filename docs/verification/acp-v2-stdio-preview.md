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
