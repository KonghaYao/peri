# MCP capability packages

## Scope

`mcp-packages/` contains MCP capability implementations: shared behavior, configuration, web, artifact, cron, and workspace. Configuration is an independent bootstrap data plane, not a session builtin tool server; the other capability packages own their tools and handlers.

The dependency direction is inward: packages may use `peri-agent`, `peri-acp-types`, `peri-resources`, and `peri-mcp-common` as needed. They must not depend on `peri-middlewares` or ACP host implementation. `peri-middlewares` remains the composition and lifecycle host and depends on these packages.

Before changing code, explicitly read the relevant standards. Peri does not inherit a parent `CLAUDE.md` when loading a package directory.

## Data flow and boundaries

Configuration has a separate bootstrap channel owned by `peri-mcp-config`: synchronous host adapters exchange MCP requests on a dedicated runtime thread before the session tool pool exists. Only the provider performs configuration file I/O and path identity probes; consumers retain typed parsing and domain precedence. See `config/CLAUDE.md` for deployment injection and trust boundaries.

The ACP host creates the builtin instance context, selects the handler, and owns MCP transport and client lifecycle. A capability package constructs its handler and tools; `peri-mcp-common` supplies shared tool-schema conversion, tool-call result mapping, numeric parameter parsing, and process-environment locking. The host connects the handler to the client and retains readiness, cancellation, bridge, and shutdown ownership.

Production Workspace instances own their Bash task state. `peri mcp-start <instance> --stdio|--http [--host HOST] [--port PORT] [--workspace DIR]` serves any builtin instance (`web` / `artifact` / `cron` / `workspace` / `workflow`) in-process at `/mcp`; there is no separate worker binary. Standalone and the builtin dispatcher construct the same handlers via `peri-middlewares/src/mcp/builtin/{dispatch,standalone}.rs`; without an ACP host `workflow` is slot-only, so runtime-dependent calls return the typed "workflow runtime is not bound" error. For Workspace, both paths construct `WorkspaceMcpServer::standalone`; the ACP session does not inject its task manager or a completion callback. Background calls return MCP Tasks handles. Session observation, cancellation and recovery target semantics are defined in `../docs/design/session-async-tasks.md`. `WorkspaceInstanceInput` is a legacy test-only slot: production paths pass `None`.

Host wire, pool, bridge, readiness, and shutdown tests belong in `peri-middlewares`. Package tests cover the capability behavior that can run without host-private APIs. Do not add a dependency from a capability package back to the host to move a host test.

## Task routing

| Task | Package entry points |
| --- | --- |
| ChatGPT plan model authentication | `credentials/src/model_auth/mod.rs`; host-only MCP, Unix credential store and lifecycle tests: [package index](../docs/code-index/mcp-packages.md). |
| Configuration bootstrap, file sources, atomic save and shared path authority | `config/CLAUDE.md`, `config/src/{lib,client,server}.rs`; DTOs `peri-acp-types/src/configuration.rs`; isolated MCP channel available before session tool-pool startup |
| OAuth credentials bootstrap | `credentials/src/{lib,server,client}.rs`: trusted bootstrap MCP `CustomRequest`, provider injected by deployment; public types `peri-acp-types/src/oauth_credentials.rs`, local/remote providers in Resources reuse the configured database and cached machine ID with `mcp_oauth_credentials`; `peri-acp/src/host/assemble.rs` injects `credentialsClient` into the OAuth pool independently of the tool pool awaiting authorization. Native uses a dedicated worker thread; Emscripten schedules the same MCP worker on its event loop. Both retain runtime tracing; the native worker inherits the caller tracing dispatcher. Pending auth / shared-DB / refresh acceptance: [2026-09 history](../spec/history/2026-09.md) (2026-09-30 entry); provider routes: [Resources index](../docs/code-index/peri-resources.md). |
| Shared MCP tool schema, server info, call mapping, failure projection, strict numeric parsing, process env lock, Workspace task owner metadata | `common/src/{helpers,numeric,failure,result_mapping,process_env}.rs`; optional owner contract re-exported from `peri-mcp-core/src/task_owner.rs` |
| Local shell execution, process-tree ownership, tee and persisted output | `common/src/{shell,shell_executor,shell_output}.rs`; `create_local_task_manager()` injects the concrete executor into Agent's lifecycle manager |
| Shared Agent definition types and pure Markdown/YAML parsing | `peri-mcp-core/src/agent_definition/` (re-exported by `common`); local discovery stays in workspace resources, web publishes its own builtin agent definitions in `web/src/agents.rs`; source trust, approval and execution policy stay in the host |
| Web search and fetch tools and handler | `web/src/{server,web_search,web_fetch,agents}.rs`; `agents/web-researcher.md` is published as `agent://builtin/web-researcher/agent.md` |
| Artifact conversion/upload tool and handler | `artifact/src/{server,tool,client}.rs` |
| Cron scheduling and tools/handler | `cron/src/{scheduler,tools,server}.rs` |
| Workspace handler and task owner | `workspace/src/{workspace,shell_tasks}.rs` |
| Workspace resource provider (skills / agents / project instructions), resource input, URI/`_meta` contract | `workspace/src/resources/`; contract types in `peri-acp-types/src/workspace_resources.rs` (skills extension key: `peri-acp-types/src/skills.rs::SKILLS_EXTENSION_ID`). Owns the local skill reads plus the `skills/list` / `skills/get` manifest and per-file digest (J5); the package registers **no skill tools** — `SkillTool` / `DiscoverSkillsTool` stay in the host and aggregate across origins (J3) |
| Workspace filesystem behavior | `workspace/src/filesystem/` |
| Image attachments and full text reads for host observers | `workspace/src/{image,file_observation}.rs`; `image/read` and `workspace/readText` custom requests, not model tools |
| Host output artifacts and attribution branches | `workspace/src/{output_store,git_branch}.rs`; `output/store` and `workspace/gitBranch` custom requests; `peri-output://` resources bind artifacts to one Workspace instance; returned paths belong to the tool environment |
| Bash execution and description | `workspace/src/terminal.rs`, `workspace/src/descriptions/bash.md` |
| Builtin selection, host context, MCP pool/client/transport, ACP adapter, readiness, bridge, shutdown | `peri-middlewares/src/mcp/` and `peri-middlewares/src/assembly.rs` |

## Stable invariants

- Shared behavior has one implementation in `peri-mcp-common`. Keep diagnostic-preserving error projection, argument defaults, schema conversion, and declared tool ordering consistent across packages.
- `server_info(name, version)` receives the capability package's version; the common package version must not appear as the server implementation version.
- Output persistence uses `peri_mcp_common::shell`; pure byte truncation and timeout policy remain in `peri_agent::agent::async_tasks`. Workspace creates its own local shell manager and exposes task status through MCP Tasks. The ACP session task manager remains separate. Host MCP bridge/resource truncation still invokes common persistence in the host process; this seam does not provide remote output storage or cross-machine Read addressing.
- A package owns its handler and tools. The host owns handler-to-client wiring, readiness admission, task supervision, cancellation delivery, and orderly shutdown. Preserve these boundaries when changing call behavior.
- Workspace background Bash and standard MCP tasks carry an optional direct owner (`_meta["peri/taskOwner"]`); they do not carry Store execution tokens or generations. Executor uniqueness and instance replacement belong to `peri-sdk`; Workspace startup neither creates an ownership marker nor rejects existing ones. Task identity, status, and cancellation are owner-filtered when an owner is present; no token, epoch, or close/open lifecycle remains.
- Workspace tools retain their existing schema, names, declaration order, and cwd binding. Background Bash execution and its cleanup belong to the Workspace instance; Peri observes Tasks through `tasks/get` and `subscriptions/listen`.
- Agent definitions come from `agent://.../agent.md` resources. Local sources are any host-bound builtin instance and an explicitly selected `WorkspaceRemote`; other servers' same-scheme resources are remote. `web-researcher` is published by the `web` instance.
- Preserve direct/deferred visibility and approval behavior. Follow `ARC-MIDDLEWARE-001`, `ARC-CAPABILITY-CLOSURE-001`, `ARC-TOOLS-001`, `ARC-CANCEL-001`, and `ARC-HOST-SHUTDOWN-001` where applicable; the standards are authoritative.

## Target commands

From the repository root:

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-mcp-common -p peri-mcp-web -p peri-mcp-artifact -p peri-mcp-cron -p peri-mcp-workspace
./scripts/cargo-rmcp-patched.sh test --locked -p peri-mcp-common -p peri-mcp-web -p peri-mcp-artifact -p peri-mcp-cron -p peri-mcp-workspace --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::workspace_builtin_tests
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::workspace_recovery_tests
```

Use exact test module paths when targeting an individual host test module. For the full test scope, host lifecycle and isolation contracts remain in `peri-middlewares` and ACP; package tests do not replace those contracts.

## Standards and references

- Start with [standards index](../docs/standards/index.md).
- Cross-crate, tool visibility, cancellation, and host lifecycle changes: [architecture contracts](../docs/standards/architecture-contracts.md), plus [peri-middlewares guide](../peri-middlewares/CLAUDE.md).
- Rust changes: [Rust standards](../docs/standards/rust.md).
- Test scope and evidence: [testing standards](../docs/standards/testing.md).
- Host entry points: [peri-middlewares code index](../docs/code-index/peri-middlewares.md).
