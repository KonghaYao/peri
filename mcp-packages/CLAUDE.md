# MCP capability packages

## Scope

`mcp-packages/` contains MCP capability implementations: shared behavior, configuration, web, artifact, cron, LSP, and workspace. Configuration is an independent bootstrap data plane, not a session builtin tool server; the other capability packages own their tools and handlers. The LSP package also owns the LSP client and host-shared pool implementation; the host retains configuration injection, readiness admission, process supervision, and shutdown authority.

The dependency direction is inward: packages may use `peri-agent`, `peri-acp-types`, `peri-resources`, and `peri-mcp-common` as needed. They must not depend on `peri-middlewares` or ACP host implementation. `peri-middlewares` remains the composition and lifecycle host and depends on these packages.

Before changing code, explicitly read the relevant standards. Peri does not inherit a parent `CLAUDE.md` when loading a package directory.

## Data flow and boundaries

Configuration has a separate bootstrap channel owned by `peri-mcp-config`: synchronous host adapters exchange MCP requests on a dedicated runtime thread before the session tool pool exists. Only the provider performs configuration file I/O and path identity probes; consumers retain typed parsing and domain precedence. See `config/CLAUDE.md` for deployment injection and trust boundaries.

The ACP host creates the builtin instance context, selects the handler, and owns MCP transport and client lifecycle. A capability package constructs its handler and tools; `peri-mcp-common` supplies shared tool-schema conversion, tool-call result mapping, numeric parameter parsing, and process-environment locking. For LSP, the package constructs the host-scoped pool and the handler owns the same injected `Arc`; the host still retains the shutdown handle and invokes bounded shutdown. The host connects the handler to the client and retains readiness, cancellation, bridge, and shutdown ownership.

Workspace's `WorkspaceInstanceInput` carries the session's task manager and background completion callback into `WorkspaceMcpServer`. The host supplies it before builtin initialization. Missing input is a supported degraded mode; it does not hide the workspace tools. The input does not transfer lifecycle ownership to the package.

Host wire, pool, bridge, readiness, and shutdown tests belong in `peri-middlewares`. Package tests cover the capability behavior that can run without host-private APIs. Do not add a dependency from a capability package back to the host to move a host test.

## Task routing

| Task | Package entry points |
| --- | --- |
| Configuration bootstrap, file sources, atomic save and shared path authority | `config/CLAUDE.md`, `config/src/{lib,client,server}.rs`; DTOs `peri-acp-types/src/configuration.rs`; isolated MCP channel available before session tool-pool startup |
| OAuth credentials bootstrap | `credentials/src/{lib,server,client}.rs`: trusted bootstrap MCP `CustomRequest`, provider injected by deployment; public types `peri-acp-types/src/oauth_credentials.rs`, local/remote providers in Resources reuse the configured database and cached machine ID with `mcp_oauth_credentials`; `peri-acp/src/host/assemble.rs` injects `credentialsClient` into the OAuth pool independently of the tool pool awaiting authorization. SDK payload-log isolation: `credentials/src/client.rs` worker runtime under `NoSubscriber` / `with_default`, without changing production main runtime logging. Completed implementation, validation evidence and pending auth/shared-DB/refresh acceptance: [active assessment](../spec/issues/2026-09-30-p2-filesystem-implementation-assessment.md); provider routes: [Resources index](../docs/code-index/peri-resources.md). |
| Shared MCP tool schema, server info, call mapping, failure projection, strict numeric parsing, process env lock | `common/src/{helpers,numeric,failure,result_mapping,process_env}.rs` |
| Local shell execution, process-tree ownership, tee and persisted output | `common/src/{shell,shell_executor,shell_output}.rs`; `create_local_task_manager()` injects the concrete executor into Agent's lifecycle manager |
| Shared Agent definition types and pure Markdown/YAML parsing | `common/src/agent_definition/`; local discovery stays in workspace resources, while source trust, approval and execution policy stay in the host |
| Web search and fetch tools and handler | `web/src/{server,web_search,web_fetch}.rs` |
| Artifact conversion/upload tool and handler | `artifact/src/{server,tool,client}.rs` |
| Cron scheduling and tools/handler | `cron/src/{scheduler,tools,server}.rs` |
| LSP tool and result formatting | `lsp/src/{tool,formatters,server}.rs` |
| Workspace handler and session input | `workspace/src/{workspace,input}.rs` |
| Workspace resource provider (skills / agents / project instructions), resource input, URI/`_meta` contract | `workspace/src/resources/`; contract types in `peri-acp-types/src/workspace_resources.rs` (skills extension key: `peri-acp-types/src/skills.rs::SKILLS_EXTENSION_ID`). Owns the local skill reads plus the `skills/list` / `skills/get` manifest and per-file digest (J5); the package registers **no skill tools** — `SkillTool` / `DiscoverSkillsTool` stay in the host and aggregate across origins (J3) |
| Workspace filesystem behavior | `workspace/src/filesystem/` |
| Image attachments and full text reads for host observers | `workspace/src/{image,file_observation}.rs`; `image/read` and `workspace/readText` custom requests, not model tools |
| Host output artifacts and attribution branches | `workspace/src/{output_store,git_branch}.rs`; `output/store` and `workspace/gitBranch` custom requests; `peri-output://` resources bind artifacts to one Workspace instance; returned paths belong to the tool environment |
| Bash execution and description | `workspace/src/terminal.rs`, `workspace/src/descriptions/bash.md` |
| Builtin selection, host context, MCP pool/client/transport, ACP adapter, readiness, bridge, shutdown | `peri-middlewares/src/mcp/` and `peri-middlewares/src/assembly.rs` |

## Stable invariants

- Shared behavior has one implementation in `peri-mcp-common`. Keep safe error projection, argument defaults, schema conversion, and declared tool ordering consistent across packages.
- `server_info(name, version)` receives the capability package's version; the common package version must not appear as the server implementation version.
- Output persistence uses `peri_mcp_common::shell`; pure byte truncation and timeout policy remain in `peri_agent::agent::async_tasks`. Agent's `TaskManager::new()` has no shell execution environment; local hosts use `peri_mcp_common::create_local_task_manager()`. Session admission and cleanup evidence remain Agent-owned. Host MCP bridge/resource truncation still invokes common persistence in the host process; this seam does not provide remote output storage or cross-machine Read addressing.
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

## W5（2026-09-29，提交 f66bd251）

**W5 交付事实（2026-09-29，提交 f66bd251）**：① `McpAgentRegistry` 增会话可见性 + A24 关闭集 + host-assigned 来源（`AgentSource::{Local{scope,plugin},Remote}`，本地只认真实 builtin `workspace` 实例）；② builtin agent 资产迁 `mcp-packages/workspace/src/resources/builtin/agents/*.md`（`BUILTIN_AGENTS` 静态表，`agent://builtin/{id}/agent.md`），`peri-middlewares/src/subagent/built-in/*.md` 与 `built_in_agents.rs` 删除；③ 定义加载（`subagent/tool/definitions.rs`）与 workflow `resolve_agent_definition`（改 async，#[async_trait]）统一走 registry；④ `{{available_agents}}` 改由 `AgentCatalogPort::catalog`（`host_ports::AgentCatalogProvider` 绑定同一 registry，装配点 `preparation.rs` downcast）；⑤ 项目指令走 `peri-instruction://workspace/{main|local}`（P4 读取 → `FrozenInstructions` → 冻结构建），`AgentsMdMiddleware` 纯 adapter（无 `std::fs`），excludes 迁 provider 输入 `instruction_excludes`；⑥ 删除：`AgentDefineMiddleware` 模块与 `ChainSlot::AgentDefine`（蓝本槽位 21→20，AskUser/Permission 位置前移 1）、`scan_agents*`、`ReadDefinition`/candidate_paths、`SkillsPort`（→ `AgentCatalogPort`）、`plugin_agent_dirs`/`claude_md_excludes` 死管线。 证据：`cargo check --workspace --all-targets` 0 error/0 warning；`-p peri-mcp-workspace --lib` 368 passed；`-p peri-middlewares --lib subagent::tool` 107 passed；`mcp::agent_registry` 10 passed；`assembly::tests` 36 passed；fmt/layer-imports/diff-check 均 EXIT 0。未完成：验收缺口用例（关闭矩阵 E2E 已有 1 条、未知字段/ excludes 各 1 条）、全量 middlewares/acp 汇总行与 clippy（由协调者统一跑）、6 处文档的完整展开。

**W5 补正（协调者审查裁决后，2026-09-29）**：① **F2**：workflow 面显式拒绝 `mcp__*` 远端 id（该面没有批准 seam；`resolve_agent_definition_via_registry`）。② **F3**：远端 agent 的 `skills` 恢复清空（技能级批准面不存在 —— W2b 结论 ⇒ 声明不构成隐式授权）；本地受信来源仍保留（X3-A）。③ **F4**：恢复 `{cwd}/agents` 为第二个 project 根（顺序在 `.claude/agents` 之后，与迁移前候选序一致）；登记扩张：`{cwd}/agents/*` 现在也进 `{{available_agents}}` 目录（迁移前只可显式加载）——「可激活即可发现」。④ **F5**：`peri-acp/src/host/workspace.rs` 拆出 `workspace_resources.rs`（1012 → 910 + 111 行，STD-SIZE-001 达标）；`session_lifecycle.rs`(1544，HEAD 1531)、`session_data.rs`(1278)、`peri-agent/src/agent/stages/mod.rs`(1011) 为**存量**超限（`check-file-size.sh` 报源码 3 / 测试 29），非本波引入，另立任务。⑤ **F6**：agent 名按来源拆分校验——本地走契约段校验（`_`/大写历史名可用，拒绝时 warn）+ 非 `mcp__` 前缀，远端沿用 HEAD 严格集。⑥ **F8**：关闭/断连的宿主绑定 builtin `workspace` 句柄**整体跳过**，不再投影成 `mcp__workspace__*` 远端条目。⑦ **F11**：DiscoverMCP 的 agent 投影绑定会话与 `SubAgentMiddleware` 关闭位（装配点从同一份 `meta_harness_disabled` 派生，单一来源）。⑧ **F13**：同 id 两形态优先级（目录形态先）显式化并加断言。⑨ **F14 纠错**：`plugin_agent_dirs` 在迁移前是**活管线**（`assemble.rs` → `frozen.rs` 喂 `scan_agents_detailed` 与 SubAgent loader），本波改为 provider 插件根输入后参数整体退场——不是「死管线」。⑩ 登记：symlink agent 定义不再可发现（X3 存量失效面，与技能侧并列）；指令链槽关闭不门控 P4 读取（与 W4b 技能面同构，行为保留）；legacy 首次接纳无执行环境 ⇒ 指令与技能摘要均不可得（J2 §3.1，新增 warn 信号）；preload 缺口报告强度属 W6。

**W5 全量链与 flake 收口（协调者，2026-09-29）**：① acp 首轮全量抓到 1 例失败：`host::requests::tests::frozen_cases::test_session_load_cold_host_restores_original_frozen_prompt`（创建期 `claude_md()` 为 `None`）。根因经确定性实验钉死（`PERI_MCP_BUILTIN=off` 稳定复现）：`BuiltinInjectionPolicy::from_env()`（`peri-middlewares/src/mcp/config.rs`）在配置加载期读**进程级 env**，开关组用例在 `#[serial]` 临界区内置 `off`（`mcp_v4_builtin_test.rs:81`、`mcp_v4_wave2_test.rs:731`、`mcp_v4_startup_test.rs:142`），而 `serial_test` 只互斥标注者——读侧 4 个用例漏标 ⇒ 并行窗口内池无 `workspace` 句柄且 `initPhase` 已收口 `ready` ⇒ 资源面按 X4 静默缺席（**生产语义正确，缺陷在测试隔离**；owner 复跑另打中 `prepared_tests::new_session_persists_frozen_bytes_from_its_single_preparation`，同一形状）。修复：`prepared_test.rs` ×2、`requests_frozen_cases_test.rs` ×1、`requests_workspace_cases_test.rs` ×1 补 `#[serial]`（`TEST-HERMETIC-001` 读侧闭合；零断言放宽、零生产代码改动、未改 10s 上界；套件 74.4s → 77.4s）。此前记「已知 flake」的 `workspace_cases::worktree_new_resources_use_the_target_directory` 同根因、同批修复（升为已定缺陷）。② 最终链：`-p peri-acp --lib` **775 passed / 0 failed**（EXIT=0）；`-p peri-mcp-workspace --lib` **372 passed / 0 failed / 1 ignored**（EXIT=0）；`clippy --workspace --all-targets -- -D warnings`、`fmt --all --check`、`check-layer-imports.sh`（20 规则 / 0 违规）、`git diff --check` 全 EXIT=0；`check-file-size.sh` EXIT=1（源码 3 / 测试 29，均存量）。③ 存量负载敏感 flake 登记（非本波引入，未修复）：`mcp::workspace_recovery_tests::external_source_keeps_120_second_deadline_and_cancels_execution` —— `-p peri-middlewares --lib` 首轮 1539 passed / 1 failed（`assert_process_gone` 5s 上界超时，`workspace_recovery_test.rs:157`；文件自 `fb54f614` 起未改动、用例自身构造 ≥120s），首轮失败证据保留；单测复跑 1 passed（120.0s）、全量复跑 **1540 passed / 0 failed / 4 ignored**（EXIT=0）。④ 约定（`peri-acp/CLAUDE.md` Verify）：触碰 builtin `workspace` 资源面的用例必须与进程级开关组同键 `#[serial]`。
