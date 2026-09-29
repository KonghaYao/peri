# MCP capability packages

## Scope

`mcp-packages/` contains the builtin MCP server implementations split by capability: shared behavior, web, artifact, cron, LSP, and workspace. Each package owns its capability's tools and server handler. The LSP package also owns the LSP client and host-shared pool implementation; the host retains configuration injection, readiness admission, process supervision, and shutdown authority.

The dependency direction is inward: packages may use `peri-agent`, `peri-acp-types`, `peri-resources`, and `peri-mcp-common` as needed. They must not depend on `peri-middlewares` or ACP host implementation. `peri-middlewares` remains the composition and lifecycle host and depends on these packages.

Before changing code, explicitly read the relevant standards. Peri does not inherit a parent `CLAUDE.md` when loading a package directory.

## Data flow and boundaries

The ACP host creates the builtin instance context, selects the handler, and owns MCP transport and client lifecycle. A capability package constructs its handler and tools; `peri-mcp-common` supplies shared tool-schema conversion, tool-call result mapping, numeric parameter parsing, and process-environment locking. For LSP, the package constructs the host-scoped pool and the handler owns the same injected `Arc`; the host still retains the shutdown handle and invokes bounded shutdown. The host connects the handler to the client and retains readiness, cancellation, bridge, and shutdown ownership.

Workspace's `WorkspaceInstanceInput` carries the session's task manager and background completion callback into `WorkspaceMcpServer`. The host supplies it before builtin initialization. Missing input is a supported degraded mode; it does not hide the workspace tools. The input does not transfer lifecycle ownership to the package.

Host wire, pool, bridge, readiness, and shutdown tests belong in `peri-middlewares`. Package tests cover the capability behavior that can run without host-private APIs. Do not add a dependency from a capability package back to the host to move a host test.

## Task routing

| Task | Package entry points |
| --- | --- |
| Shared MCP tool schema, server info, call mapping, failure projection, strict numeric parsing, process env lock | `common/src/{helpers,numeric,failure,result_mapping,process_env}.rs` |
| Web search and fetch tools and handler | `web/src/{server,web_search,web_fetch}.rs` |
| Artifact conversion/upload tool and handler | `artifact/src/{server,tool,client}.rs` |
| Cron scheduling and tools/handler | `cron/src/{scheduler,tools,server}.rs` |
| LSP tool and result formatting | `lsp/src/{tool,formatters,server}.rs` |
| Workspace handler and session input | `workspace/src/{workspace,input}.rs` |
| Workspace resource provider (skills / agents / project instructions), resource input, URI/`_meta` contract | `workspace/src/resources/`; contract types in `peri-acp-types/src/workspace_resources.rs` (skills extension key: `peri-acp-types/src/skills.rs::SKILLS_EXTENSION_ID`) |
| Workspace filesystem behavior | `workspace/src/filesystem/` |
| Bash execution and description | `workspace/src/terminal.rs`, `workspace/src/descriptions/bash.md` |
| Builtin selection, host context, MCP pool/client/transport, ACP adapter, readiness, bridge, shutdown | `peri-middlewares/src/mcp/` and `peri-middlewares/src/assembly.rs` |

## Stable invariants

- Shared behavior has one implementation in `peri-mcp-common`. Keep safe error projection, argument defaults, schema conversion, and declared tool ordering consistent across packages.
- `server_info(name, version)` receives the capability package's version; the common package version must not appear as the server implementation version.
- Output persistence and byte truncation use the canonical `peri_agent::agent::async_tasks` functions. Do not recreate aliases or copy their rules here.
- A package owns its handler and tools. The LSP package additionally owns its client/pool implementation and the builtin handler holds the injected host-scoped pool; the host owns pool visibility, shutdown invocation, readiness admission, task supervision, cancellation delivery, and orderly shutdown. Preserve these shared-pool boundaries when changing call behavior.
- Workspace tools retain their existing schema, names, declaration order, cwd binding, timeout and cancellation behavior. `WorkspaceInstanceInput` is session-scoped; the host remains its source and lifecycle owner.
- Preserve direct/deferred visibility and approval behavior. Follow `ARC-MIDDLEWARE-001`, `ARC-CAPABILITY-CLOSURE-001`, `ARC-TOOLS-001`, `ARC-CANCEL-001`, and `ARC-HOST-SHUTDOWN-001` where applicable; the standards are authoritative.

## Target commands

From the repository root:

```bash
cargo build -p peri-mcp-common -p peri-mcp-web -p peri-mcp-artifact -p peri-mcp-cron -p peri-mcp-lsp -p peri-mcp-workspace
cargo test -p peri-mcp-common -p peri-mcp-web -p peri-mcp-artifact -p peri-mcp-cron -p peri-mcp-lsp -p peri-mcp-workspace --lib
cargo test -p peri-middlewares --lib -- mcp::workspace_builtin_tests
cargo test -p peri-middlewares --lib -- mcp::workspace_recovery_tests
```

Use exact test module paths when targeting an individual host test module. For the full test scope, host lifecycle and isolation contracts remain in `peri-middlewares` and ACP; package tests do not replace those contracts.

## Standards and references

- Start with [standards index](../docs/standards/index.md).
- Cross-crate, tool visibility, cancellation, and host lifecycle changes: [architecture contracts](../docs/standards/architecture-contracts.md), plus [peri-middlewares guide](../peri-middlewares/CLAUDE.md).
- Rust changes: [Rust standards](../docs/standards/rust.md).
- Test scope and evidence: [testing standards](../docs/standards/testing.md).
- Host entry points: [peri-middlewares code index](../docs/code-index/peri-middlewares.md).
