# Configuration data plane

## Scope

`peri-mcp-config` owns configuration source I/O and deployment-path authority. It is independent of the session tool pool: configuration selects which tool servers to start, so bootstrapping it through that pool would create a dependency cycle.

Read `../../../docs/standards/{architecture-contracts,rust,testing,documentation}.md` explicitly before changes. Contract DTOs live in `peri-acp-types/src/configuration.rs`.

## Data flow

Peri `peri-config::source` adapters and dedicated capability input adapters → synchronous `ConfigurationClient` → a dedicated runtime thread → MCP `config/execute` → `ConfigurationMcpServer` → source environment filesystem / named environment.

The default adapter uses an in-process duplex transport, not direct filesystem calls. Deployment can call `install_client(ConfigurationClient::connect_tcp(...))` before the first configuration access. Installation after initialization fails; there is no dual source or filesystem fallback. TCP transport has no authentication or TLS here and must remain inside a deployment-controlled trusted channel.

## Boundaries

- This is a bootstrap control capability, not a model tool or an entry in the session builtin registry. Do not tie availability to WorkspaceMiddleware or `PERI_MCP_BUILTIN`.
- The provider owns reads, named environment lookups, existence and canonical identity probes, atomic replacement, home/cwd lookup and global-path override. Environment values come from the configuration provider, never from a fallback on the compute host. Requests may address deployment-supplied paths; this is not a sandbox or an untrusted public file service.
- `peri-config` owns typed parsing, defaults, domain precedence, scoped snapshots/revisions, explanation and update acceptance for migrated settings/provider/MCP/Langfuse/UI domains and resource-flag projections. This provider only supplies input I/O; it never publishes effective configuration. Read `../../../peri-config/CLAUDE.md` and `../../../docs/design/configuration-authority.md`. Plugin lifecycle, hook formats, OS execution environment and storage locator/credentials remain dedicated capability boundaries.
- `WriteTextIfUnchanged` compares expected bytes before replacement: missing and empty are distinct. Compare and replace share the process lock and a target-path cross-process file lock with atomic writes. It coordinates cooperating writers, not uncooperative editors or a multi-file transaction. A false result is a conflict, not a successful save.
- Missing files and other I/O failures are distinct. Unknown identity must not turn into a distinct configuration layer or a duplicate hook source. No failed write may report success.
- Synchronous startup and current-thread Tokio callers must not require their own executor to progress the MCP exchange. Requests and initialization are bounded; a timed-out write may already have entered blocking OS I/O.
- Existing infallible path-control APIs fail explicitly if the data plane is unavailable; fallible reads/writes propagate errors. Keep this distinction visible when changing deployment assembly.

## Verification

```bash
cargo test -p peri-mcp-config --lib
cargo test -p peri-mcp-config --lib -- cas_test
cargo test -p peri-config --lib -- system::tests
cargo test -p peri-config --lib -- settings::tests
cargo test -p peri-middlewares --lib -- mcp::config::
cargo test -p peri-middlewares --lib -- settings::tests
cargo test -p peri-middlewares --lib -- hooks::loader::tests
```
