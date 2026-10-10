# P0：新会话冻结提示词的 Agent 目录恒为空

状态：实现修复，等待现场验收。日期：2026-10-07。

## 观察

主 system prompt 的 `{{available_agents}}` 段在**会话创建期渲染一次**，而该次渲染发生在 Agent registry 绑定到目录端口**之前**；冻结数据在会话生命周期内不可变（ARC-FROZEN-001），于是 session/new 及其派生的会话路径上，目录段在整个生命周期恒为 `No agents currently configured`（其余路径的残留见「影响面」与「遗留决策」）。

用户可观察后果：builtin 的 `coder` / `explorer` / `general-purpose` / `plan` / `verification` / `web-researcher` 六个定义对主 Agent **不可见**；Agent 决策期看不到任何子 Agent 候选，只在外界人工点名时才会去加载定义。目录为空无任何信号——端口按设计不回落磁盘（X4/J5），空目录与「确实没有候选」不可区分。

## 证据

断链三段，均逐段读码确认：

1. **渲染**：`peri-acp/src/session/frozen.rs:124` `template.render(&env, self.inner.agent_catalog.as_ref())`，由 `build_frozen_data_with_deployment_closure` 调用，会话创建期经 `peri-acp/src/host/prepared.rs:188` `build_frozen_after_activation` 触发；session/new 在 `assemble_with_frozen` 返回之后才走到这一步（`peri-acp/src/host/requests/session_lifecycle.rs` 渲染段）。
2. **端口**：会话级 `AgentCatalogProvider`（`peri-middlewares/src/host_ports.rs:542-575`）内部是 `RwLock<Option<Arc<McpAgentRegistry>>>`；未绑定时 `catalog()` 走 `unwrap_or_default()` ⇒ 空目录。
3. **绑定**：全仓唯一绑定点是 `peri-middlewares/src/assembly/preparation.rs:55-62`（`resolve_ports` 内），由每 turn 的链装配（`peri-middlewares/src/assembly.rs:136`）调用——晚于冻结渲染。

现场快照证据（2026-10-07 查询 `~/.peri/threads/threads.db`）：

- 近期 60 个主会话 `frozen_context` 快照 **60/60** 全部含 `No agents currently configured`；子 Agent / fork 继承同一份字节（31628 字节逐字节相同）。
- 同批快照 `claude_md`（3790 字符）、`skill_summary`（1304 字符）非空 ⇒ builtin `workspace` 实例与资源面正常，排除「连接未就绪」。
- `meta_harness.built_in_subagents_enabled=true`、`disabled_middlewares=[]` ⇒ 排除关闭位。

缺陷无端到端守护：`e2e/tests/tool-cards/agent-output-position.test.ts` 只测 TUI 卡片，仓库无 catalog 内容断言。

## 影响面

| 路径 | 是否受影响 | 说明 |
| --- | --- | --- |
| session/new | **是（本次修复目标）** | 渲染早于 bind |
| load / resume / fork | 否 | 复用持久 blob，不重渲染 ⇒ 修复前已创建的会话**仍保持空目录**（见遗留决策 1） |
| legacy 首次接纳 | 是（残留，见遗留决策 2） | 接纳在准入事务之前、无执行环境 ⇒ 无资源面；且其渲染读 host 级 provider，与 turn 级会话 provider 分叉 |
| workflow agent / subagent | 否 | 运行期渲染，晚于 turn 级 bind |

## 修复

在 `assemble_with_frozen` 返回前，用当轮同一份 `cfg.mcp_pool` 与关闭集完成会话级目录端口绑定：

- `peri-middlewares/src/mcp/agent_registry.rs` 新增 `McpAgentRegistry::for_session(pool, session_id, disabled_middlewares)`，作为**会话级 registry 唯一构造入口**：会话过滤与 `SubAgentMiddleware` 关闭位一次成形，关闭位由同一份 `disabled_middlewares` 派生。会话创建期与 turn 级两个 bind 点共用该入口，杜绝「同形」只靠两处注释维持；`assembly/workflow.rs` 与 `mcp/middleware.rs` 两个**有意不同形**的构造点不为统一而改。
- `peri-middlewares/src/host_ports.rs` 新增公开函数 `bind_agent_catalog_from_pool(agent_catalog, pool, session_id, disabled_middlewares) -> bool`：pool → `McpClientPool`、端口 → `AgentCatalogProvider` 双 downcast 后 bind；任一步不成立返回 `false`（每条早退留可区分原因的 `tracing::debug!`），不构造降级实例、不新增第二来源或第二缓存。绑定经 middlewares 公开函数完成，ACP 侧仍只持 `Arc<dyn AgentCatalogPort>`（§0 依赖方向）。
- `peri-acp/src/host/workspace.rs` 的 `assemble_with_frozen` 内、`cfg` 构造与既有补丁之后、最终 `Ok(Some(...))` 返回之前调用（本函数唯一的早退是 `workspace_assembly` 为 `None`，那条路径无执行环境、也无从绑定）。该函数是 new/load/resume/fork 的共同装配点；提前绑定对恢复路径是同一份 registry 形状的等价覆盖，不改变任何路径的可见性或锁时序。

## 验证与验收

2026-10-07，均从仓库根使用 `./scripts/cargo-rmcp-patched.sh`（标准路径，未加 `--offline`）：

- `test --locked -p peri-middlewares --lib -- host_ports`：2 passed、0 failed（正例：真实线路夹具绑定后目录非空且本地面关闭位被尊重；反例：端口非本 crate 实现时返回 `false`）。
- `test --locked -p peri-acp --lib -- host::requests::tests::agent_catalog`：2 passed、0 failed（生产 `session/new` 全链：开放集冻结 prompt 列出候选；`SubAgentMiddleware` 关闭集差分不列出候选且不泄漏占位符原文，另有端口直读断言）。
- `test --locked -p peri-middlewares --lib -- assembly::tests`：37 passed、0 failed（turn 级装配与关闭集派生未因抽取回归）。
- `build --locked -p peri-acp` 与 `clippy --locked -p peri-middlewares -p peri-acp --all-targets -- -D warnings`：exit 0，无告警；`check --locked`（全 workspace 类型门）、`fmt --check -p peri-middlewares -p peri-acp`、`typos`：exit 0；`bash scripts/check-layer-imports.sh`：22 条规则、违规边 0。
- 变异验证（两次独立变异，各自逐字节还原）：摘除 `assemble_with_frozen` 的定点绑定 ⇒ 基线用例以原始症状失败（`冻结 prompt 的 available_agents 段=No agents currently configured`），关闭集差分仍绿；把关闭位硬编码为不命中 ⇒ 差分用例失败且红在端口直读断言，基线仍绿。两次变异证明测试具备鉴别力。
- 过滤词陷阱：真实模块路径是 `host::requests::tests::agent_catalog`；按文件名派生的 `host::requests_agent_catalog` 命中 0 tests 且 exit 0，不可作为通过证据。
- `peri-middlewares` 全量 `--lib`：1527 passed、77 failed。失败集中在 `subagent::tool::*`、`mcp::workspace_(recovery|builtin)_tests::*` 等本次未触及的路径（对失败文件检索被改动符号零命中），`--skip` 新增用例后失败集合逐名相同，隔离运行同样失败 ⇒ 判定为既有失败，不声称该 crate 全绿；未在 HEAD 副本上复跑，该判定含推断成分。
- 新增测试与 `assembly::tests` 覆盖本次改动路径；`test --locked -p peri-acp --lib` 全量：795 passed、0 failed（含两条新用例）。未运行 e2e（本次不要求）。
- 方法学注意：用 `cp -p` 方式还原变异会保留 mtime，cargo 可能不重编译而读到变异二进制；变异的复绿结论以显式触发重编译后的运行为准。

## 遗留决策（不在本次范围）

1. **已存在的会话**：冻结数据不可变 ⇒ 修复前创建的 thread 在修复后仍渲染空目录。是否需要「一次性重冻」迁移（例如快照加渲染修订位，仅在旧版本快照上重渲染一次）属产品决策，本次不擅自改动冻结契约。
2. **legacy 首次接纳**：`prepare_legacy` 在接纳事务前构建 frozen，既无执行环境，又与 turn 级会话 provider 分叉；要覆盖需把 frozen 构建挪到装配之后，属结构性改动，另立任务。
3. **次生问题**：catalog 渲染不含 description（`peri-acp/src/prompt/mod.rs:274-294` 只输出 `- {id} [{tier}] [{access}]`），而 `peri-acp/src/prompt/prompt_sections_test.rs:331-333` 的设计意图是「调度建议由 catalog 的 id/description 承载」；`peri-acp/prompts/sections/11_subagent.md:22` 的 "**Do NOT** use sub-agents for simple file reads, searches" 与 `explorer` 的定位正面冲突。两者均影响 Agent 是否真的会去用子 Agent，但与本 P0 的断链根因独立，另行跟踪。
