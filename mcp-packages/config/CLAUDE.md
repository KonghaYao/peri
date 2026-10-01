# Configuration data plane

## Scope

`peri-mcp-config` owns configuration source I/O and deployment-path authority. It is independent of the session tool pool: configuration selects which tool servers to start, so bootstrapping it through that pool would create a dependency cycle.

Read `../../../docs/standards/{architecture-contracts,rust,testing,documentation}.md` explicitly before changes. Contract DTOs live in `peri-acp-types/src/configuration.rs`.

## Data flow

ACP and middleware configuration adapters → synchronous `ConfigurationClient` → a dedicated runtime thread → MCP `config/execute` → `ConfigurationMcpServer` → configuration environment filesystem.

The default adapter uses an in-process duplex transport, not direct filesystem calls. Deployment can call `install_client(ConfigurationClient::connect_tcp(...))` before the first configuration access. Installation after initialization fails; there is no dual source or filesystem fallback. TCP transport has no authentication or TLS here and must remain inside a deployment-controlled trusted channel.

## Boundaries

- This is a bootstrap control capability, not a model tool or an entry in the session builtin registry. Do not tie availability to WorkspaceMiddleware or `PERI_MCP_BUILTIN`.
- The provider owns reads, named environment lookups, existence and canonical identity probes, atomic replacement, home/cwd lookup and global-path override. Environment values come from the configuration provider, never from a fallback on the compute host. Requests may address deployment-supplied paths; this is not a sandbox or an untrusted public file service.
- Consumers retain their typed configuration parsing, validation, source precedence and domain projections. Plugin installation/marketplace lifecycle and P2 workspace addressing remain separate work.
- Missing files and other I/O failures are distinct. Unknown identity must not turn into a distinct configuration layer or a duplicate hook source. No failed write may report success.
- Synchronous startup and current-thread Tokio callers must not require their own executor to progress the MCP exchange. Requests and initialization are bounded; a timed-out write may already have entered blocking OS I/O.
- Existing infallible path-control APIs fail explicitly if the data plane is unavailable; fallible reads/writes propagate errors. Keep this distinction visible when changing deployment assembly.

## Verification

```bash
cargo test -p peri-mcp-config --lib
cargo test -p peri-acp --lib -- provider::store::store_tests
cargo test -p peri-middlewares --lib -- mcp::config::
cargo test -p peri-middlewares --lib -- settings::tests
cargo test -p peri-middlewares --lib -- hooks::loader::tests
```
