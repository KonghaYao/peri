# Bounded validation: 2026-09-13

This is a read-only validation of the current SQLite snapshot through `analyzeDatabase` and `evidenceForMessage`. The filter is visible root threads with `thread.created_at >= 2026-09-01T00:00:00.000Z` and `< 2026-09-13T00:00:00.000Z`; timestamps are UTC. No message body, arguments, user path, title, or secret is exported.

The run selected 204 threads, 33,961 messages, and 19,891 calls. All 19,891 calls had paired known results; there were no missing results, orphan results, duplicate IDs, or parse issues. The source fingerprint is `c22d493aab76d24e24925b6352b5137cf5b3535563b6aa65c8d73904834da1e1`.

| label | calls | errors | called threads | error affected threads |
| --- | ---: | ---: | ---: | ---: |
| StrReplace | 19 | 19 | 6 | 6 |
| ExecuteExtraTool→Workflow | 61 | 24 | 16 | 12 |
| Agent | 347 | 41 | 56 | 17 |

I reviewed 9 bounded error cases: three independent threads per label, five unique threads after overlap. Each case used a radius of three normalized records and included content only for local classification; the saved artifacts contain IDs and bounded error metadata, not the content.

The three sampled StrReplace failures all classify as `tool_not_found`. The current source registers `EditFileTool` under the effective name `Edit` in `FilesystemMiddleware`; that makes a historical-name compatibility check a concrete candidate. It does not yet prove whether the database calls came from a stale prompt, an alias migration gap, or a provider artifact. The relevant entry points are `peri-middlewares/src/middleware/filesystem.rs`, `peri-middlewares/src/tools/filesystem/edit.rs`, and `peri-middlewares/src/tool_search/execute_tool.rs`.

The three sampled Workflow failures split into two preflight input diagnostics (export restriction and syntax error) and one runner spawn/RPC failure. They should remain separate categories. The next check is to classify the full 24 errors and add fixtures around `preflight_validate_script` and runner/RPC startup. The relevant entry points are `peri-workflow/src/tool/preflight.rs`, `peri-workflow/src/tool.rs`, `peri-workflow/src/rpc.rs`, and `peri-workflow/src/error.rs`.

The three sampled Agent failures report model stream interruption or retry exhaustion. Each has later successful activity in its bounded context. The SQLite evidence does not identify the provider or root cause, so this remains unverified pending provider/runtime attempt diagnostics. The relevant entry points are `peri-acp/src/provider/mod.rs`, `peri-agent/src`, and `peri-model/src`.

The observed follow-up success in all nine cases is evidence against calling these sampled events permanent inability. No cancellation event was available in this validation, so cancellation semantics and cancellation-as-failure rates remain unverified.

The machine-readable observations, IDs, source identity, status labels, and improvement queue are in [findings.json](./findings.json).
