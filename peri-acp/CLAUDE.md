# peri-acp

## Scope

`peri-acp` 负责 ACP 服务层：session 生命周期、prompt 构建、Agent 装配入口与事件映射/发送；不实现 TUI 组件。中间件链序蓝本位于 Agent 层（见 `../peri-agent/CLAUDE.md`），具体装配位于 `../peri-middlewares/src/assembly.rs`；本层构造宿主装配上下文。Langfuse 观测已随 L4 迁出至 `peri-controller`（事件流旁路消费者，见 `../peri-controller/`），本层仅在事件协议化前分支调用 bridge（`src/event/forwarder.rs`）。

## 数据流

`ACP request → SessionManager → frozen session data / prompt → Agent 层 session 工厂（装配）→ run_react_loop → ExecutorEvent → event mapper / event sink → SessionUpdate 或扩展通知 → client`。Langfuse 经 `peri-controller` 的 `LangfuseBridge`（事件流旁路消费者）在协议化前分支消费事件；不改变客户端事件路径。

## 任务路由

| 任务 | 优先读取 |
| --- | --- |
| session、事件、Prompt、工具、中间件、secret | `../docs/standards/architecture-contracts.md` |
| Rust、async 与 doc tests | `../docs/standards/rust.md` |
| 测试位置与覆盖要求 | `../docs/standards/testing.md` |
| middleware 具体链顺序 | `../peri-middlewares/CLAUDE.md` 与 `../peri-agent/src/session/factory.rs` |
| TUI 通知消费 | `../peri-tui/CLAUDE.md` 与 `../docs/standards/tui.md` |

不通过导入扩展默认上下文；需规则时按表显式读取。

## 稳定不变量

- ACP 只驱动当前进程内执行，不提供持久 execution admission、Work/control 协议或冷恢复，不要求 SDK reverse admission，不向工具继承执行 token。session/load 与 resume 仍校验 Store 访问模式、持久化状态、binding、保存目录与 frozen；不能执行的环境明确失败，独立 history RPC 保持可读。显式 close intent、prompt 排空、SessionEnd、MCP/task 当前资源生命周期与普通关闭的持久化结清保持独立。

- 配置定义、typed 解析、provider/profile 默认值、分层合并与差异保存归 `peri-config`；`provider/{config,store}.rs` 仅 re-export。正常 `peri_config::settings::ConfigSource` 持有 `ConfigurationSystem`，固定 scope/layout，并提供 snapshot、显式 reload 与 CAS 保存；布局或校验失败不得误写全局层。见 `../peri-config/CLAUDE.md`。
- 配置输入经独立 bootstrap `peri-mcp-config` 采集，不依赖待配置的 session 工具池，不新增 daemon/model 工具；环境来自选中的 source provider，不读计算宿主 fallback。核心不反向依赖 ACP 业务实现。
- 宿主创建的 MCP pool 在初始化前绑定同一 `ConfigSource` snapshot；host Langfuse 使用该快照的 typed projection，LLM adapter 消费 core `ResolvedProvider`。保存保留同文件兄弟域；失败不 publish，显式 reload 不热替换已初始化 pool 或已冻结 session prefix。
- workspace 资源输入使用 `snapshot.resources().disable_bundled_skills`，不重新读取全局关闭位；技能回归 fixture 必须使用与生产 source 一致的选中 global 路径。需要新值时显式 reload 并重新取得 snapshot，旧 pool 固定旧 Arc，没有 hot watcher。lenient 无 authority 仅临时可读，不可写。
- `ConfigSource::save(expected_revision, &PeriConfig)` 返回 accepted snapshot；持久 configOptions / update_config 必须 candidate → 验证 → 保存成功 → 从 accepted snapshot 发布，失败不更新 live provider / agent cache、不 notify、不返回成功。revision 在编辑开始捕获，不得提交时换成最新版；远程 wire 与延迟 UI draft 的基线 token 仍需审计，见配置 active issue。
- `SessionManager` 在每条 session/new、load、resume 或 fork 路径注册 session caps；发送扩展事件前按该 session 的 caps 门控。
- 新增 `ExecutorEvent` 或 ACP 扩展事件时，覆盖发射、ACP mapper/forwarder、caps 门控（如适用）和客户端消费；不能只增加枚举或单一发送点。
- 给 Hub/Web 的事件投影必须从 canonical event 映射为版本化 allowlist DTO；不得复用包含消息、路径、输出或错误正文的 TUI 私有 `event_json`。`peri.agentActivity` 是该安全摘要面，legacy `peri.agentEvent` wire 保持独立兼容。
- session 创建时构建并复用 frozen 数据；Prompt 与 SubAgent 不得在会话中途重读导致前缀漂移。
- 生产中间件顺序以 Agent 层 session 工厂的链序蓝本为事实源（`../peri-agent/src/session/factory.rs` 的 `production_blueprint`），未经完整验证不得重排。
- Langfuse 事件只经 `peri-controller` 的 `LangfuseBridge` 统一映射进入 tracer（协议化前分支，不参与业务链路）；日志、错误和遥测不得泄露 secret。
- stdio/MPSC transport 的 pending request 由 router 统一持有：response、caller cancellation 与 terminal close 至多结算一次；终止结算当前和后续请求，连接静默不引入隐式 timeout（ARC-TRANSPORT-001）。
- `--bare` 会话仅装配 builtin `workspace` MCP 池，保留基础文件/终端及后台任务能力；跳过用户插件、外部 MCP 和 settings hooks。`PERI_MCP_BUILTIN=off` / `0` 仍显式关闭注入。
- builtin MCP 实例上下文须在 `McpClientPool::run_initialize` 前装配，后置注入会被拒绝。生产 Workspace 的后台 Bash 由 MCP 自行持有；会话侧任务投影与取消路由的目标设计见 `../docs/design/session-async-tasks.md`。
- Cron scheduler 属于会话环境：builtin 工具、宿主端口与 session bridge 使用同一份实例；共享会话注册表时只共享 continuation 入口，不覆盖 scheduler。部署的 tick 开关传入会话池，由 builtin supervisor 唯一驱动并随会话关闭。
- Host 后台任务由 non-Clone `HostTaskOwner` 持有，config/task 只持 weak `HostTaskSpawner`；MCP concrete owner 属 middlewares，ACP config 只能持 `peri-acp-types::ports::McpTaskOwnerPort`，禁止直接依赖 concrete type。transport EOF 关闭准入后取消并 drain local/manager 会话 ID 并集，再在锁外关闭 MCP。Host drain 或 MCP service-close report 超时必须报告 `Incomplete` 并保持 Closing，不得当作已经 join/Closed（ARC-HOST-SHUTDOWN-001）。
- 会话 setup（`session/new` / `load` / `resume` / `fork`）里的 `mcpServers` acp 型声明在响应写入后由 `host/requests/acp_mcp.rs` 受理（`attach_session_servers`）；`mcp/connect` 只带 client 声明的 `serverId`，因此受理顺序不能提前到响应之前。会话级服务持有连接（每个会话一个 MCP 池），入站 `mcp/message` 按 `connectionId` 定位承载会话、未知连接返回 `-32001`，内层 MCP 错误码原样透传；会话终结在 MCP 池关闭前调 `AcpMcpServerPort::close_session`（幂等）。建连是后台的：不阻塞会话建立，失败留在 MCP 池状态面（ARC-MCP-ACP-001）。

## 目标命令

```bash
cargo check -p peri-acp
cargo test -p peri-acp --lib
cargo test -p peri-acp --lib -- host::task_scope
cargo test -p peri-acp --lib -- acp_mcp
cargo test -p peri-acp --lib mapper
cargo test -p peri-controller --test langfuse_e2e
cargo test -p peri-acp --doc
```

## Verify

- session/caps 改动：运行相关 crate 测试，并人工检查所有创建、加载、恢复、fork 入口均在 session 就绪后注册 caps。
- 事件改动：运行 mapper 测试，并人工沿服务端发送点到 TUI/stdio 客户端检查新增事件覆盖；现有 mapper 测试不自动证明全链路完整。
- Prompt、middleware 或 Langfuse 改动：按 `ARC-FROZEN-001`、`ARC-MIDDLEWARE-001`、`ARC-SECRET-001` 逐项核对。
- 配置规则改动：先读 `../peri-config/CLAUDE.md` 与 `../docs/design/configuration-authority.md`；领域回归在 core，ACP 验证消费与装配接线。插件生命周期、hook 格式及存储/执行 credentials 仍遵守专属能力边界。
- 触碰 builtin `workspace` 资源面的测试（agent / 技能 / 指令 / meta 文档）：用例依赖 `PERI_MCP_BUILTIN` **默认态**，而该进程级 env 由开关组用例在 `#[serial]` 临界区内改写 ⇒ 读侧必须同键 `#[serial]`（`TEST-HERMETIC-001`「串行化」的读侧闭合；漏标时并行窗口内池无 `workspace` 句柄、资源面按 X4 静默缺席，症状是内容断言拿到 `None`/空）。

## W5（2026-09-29，提交 f66bd251）

**W5 交付事实（2026-09-29，提交 f66bd251）**：① `McpAgentRegistry` 增会话可见性 + A24 关闭集 + host-assigned 来源（`AgentSource::{Local{scope,plugin},Remote}`，本地只认真实 builtin `workspace` 实例）；② builtin agent 资产迁 `mcp-packages/workspace/src/resources/builtin/agents/*.md`（`BUILTIN_AGENTS` 静态表，`agent://builtin/{id}/agent.md`），`peri-middlewares/src/subagent/built-in/*.md` 与 `built_in_agents.rs` 删除；③ 定义加载（`subagent/tool/definitions.rs`）与 workflow `resolve_agent_definition`（改 async，#[async_trait]）统一走 registry；④ `{{available_agents}}` 改由 `AgentCatalogPort::catalog`（`host_ports::AgentCatalogProvider` 绑定同一 registry，装配点 `preparation.rs` downcast）；⑤ 项目指令走 `peri-instruction://workspace/{main|local}`（P4 读取 → `FrozenInstructions` → 冻结构建），`AgentsMdMiddleware` 纯 adapter（无 `std::fs`），excludes 迁 provider 输入 `instruction_excludes`；⑥ 删除：`AgentDefineMiddleware` 模块与 `ChainSlot::AgentDefine`（蓝本槽位 21→20，AskUser/Permission 位置前移 1）、`scan_agents*`、`ReadDefinition`/candidate_paths、`SkillsPort`（→ `AgentCatalogPort`）、`plugin_agent_dirs`/`claude_md_excludes` 死管线。 证据：`cargo check --workspace --all-targets` 0 error/0 warning；`-p peri-mcp-workspace --lib` 368 passed；`-p peri-middlewares --lib subagent::tool` 107 passed；`mcp::agent_registry` 10 passed；`assembly::tests` 36 passed；fmt/layer-imports/diff-check 均 EXIT 0。未完成：验收缺口用例（关闭矩阵 E2E 已有 1 条、未知字段/ excludes 各 1 条）、全量 middlewares/acp 汇总行与 clippy（由协调者统一跑）、6 处文档的完整展开。

**W5 补正（协调者审查裁决后，2026-09-29）**：① **F2**：workflow 面显式拒绝 `mcp__*` 远端 id（该面没有批准 seam；`resolve_agent_definition_via_registry`）。② **F3**：远端 agent 的 `skills` 恢复清空（技能级批准面不存在 —— W2b 结论 ⇒ 声明不构成隐式授权）；本地受信来源仍保留（X3-A）。③ **F4**：恢复 `{cwd}/agents` 为第二个 project 根（顺序在 `.claude/agents` 之后，与迁移前候选序一致）；登记扩张：`{cwd}/agents/*` 现在也进 `{{available_agents}}` 目录（迁移前只可显式加载）——「可激活即可发现」。④ **F5**：`peri-acp/src/host/workspace.rs` 拆出 `workspace_resources.rs`（1012 → 910 + 111 行，STD-SIZE-001 达标）；`session_lifecycle.rs`(1544，HEAD 1531)、`session_data.rs`(1278)、`peri-agent/src/agent/stages/mod.rs`(1011) 为**存量**超限（`check-file-size.sh` 报源码 3 / 测试 29），非本波引入，另立任务。⑤ **F6**：agent 名按来源拆分校验——本地走契约段校验（`_`/大写历史名可用，拒绝时 warn）+ 非 `mcp__` 前缀，远端沿用 HEAD 严格集。⑥ **F8**：关闭/断连的宿主绑定 builtin `workspace` 句柄**整体跳过**，不再投影成 `mcp__workspace__*` 远端条目。⑦ **F11**：DiscoverMCP 的 agent 投影绑定会话与 `SubAgentMiddleware` 关闭位（装配点从同一份 `meta_harness_disabled` 派生，单一来源）。⑧ **F13**：同 id 两形态优先级（目录形态先）显式化并加断言。⑨ **F14 纠错**：`plugin_agent_dirs` 在迁移前是**活管线**（`assemble.rs` → `frozen.rs` 喂 `scan_agents_detailed` 与 SubAgent loader），本波改为 provider 插件根输入后参数整体退场——不是「死管线」。⑩ 登记：symlink agent 定义不再可发现（X3 存量失效面，与技能侧并列）；指令链槽关闭不门控 P4 读取（与 W4b 技能面同构，行为保留）；legacy 首次接纳无执行环境 ⇒ 指令与技能摘要均不可得（J2 §3.1，新增 warn 信号）；preload 缺口报告强度属 W6。

**W5 全量链与 flake 收口（协调者，2026-09-29）**：① acp 首轮全量抓到 1 例失败：`host::requests::tests::frozen_cases::test_session_load_cold_host_restores_original_frozen_prompt`（创建期 `claude_md()` 为 `None`）。根因经确定性实验钉死（`PERI_MCP_BUILTIN=off` 稳定复现）：`BuiltinInjectionPolicy::from_env()`（`peri-middlewares/src/mcp/config.rs`）在配置加载期读**进程级 env**，开关组用例在 `#[serial]` 临界区内置 `off`（`mcp_v4_builtin_test.rs:81`、`mcp_v4_wave2_test.rs:731`、`mcp_v4_startup_test.rs:142`），而 `serial_test` 只互斥标注者——读侧 4 个用例漏标 ⇒ 并行窗口内池无 `workspace` 句柄且 `initPhase` 已收口 `ready` ⇒ 资源面按 X4 静默缺席（**生产语义正确，缺陷在测试隔离**；owner 复跑另打中 `prepared_tests::new_session_persists_frozen_bytes_from_its_single_preparation`，同一形状）。修复：`prepared_test.rs` ×2、`requests_frozen_cases_test.rs` ×1、`requests_workspace_cases_test.rs` ×1 补 `#[serial]`（`TEST-HERMETIC-001` 读侧闭合；零断言放宽、零生产代码改动、未改 10s 上界；套件 74.4s → 77.4s）。此前记「已知 flake」的 `workspace_cases::worktree_new_resources_use_the_target_directory` 同根因、同批修复（升为已定缺陷）。② 最终链：`-p peri-acp --lib` **775 passed / 0 failed**（EXIT=0）；`-p peri-mcp-workspace --lib` **372 passed / 0 failed / 1 ignored**（EXIT=0）；`clippy --workspace --all-targets -- -D warnings`、`fmt --all --check`、`check-layer-imports.sh`（20 规则 / 0 违规）、`git diff --check` 全 EXIT=0；`check-file-size.sh` EXIT=1（源码 3 / 测试 29，均存量）。③ 存量负载敏感 flake 登记（非本波引入，未修复）：`mcp::workspace_recovery_tests::external_source_keeps_120_second_deadline_and_cancels_execution` —— `-p peri-middlewares --lib` 首轮 1539 passed / 1 failed（`assert_process_gone` 5s 上界超时，`workspace_recovery_test.rs:157`；文件自 `fb54f614` 起未改动、用例自身构造 ≥120s），首轮失败证据保留；单测复跑 1 passed（120.0s）、全量复跑 **1540 passed / 0 failed / 4 ignored**（EXIT=0）。④ 约定（`peri-acp/CLAUDE.md` Verify）：触碰 builtin `workspace` 资源面的用例必须与进程级开关组同键 `#[serial]`。
