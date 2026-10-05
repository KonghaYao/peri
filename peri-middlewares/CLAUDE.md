# peri-middlewares

## Scope

`peri-middlewares` 承载提示词、插件、审批、SubAgent 和 MCP 宿主生命周期。链序蓝本在 `../peri-agent/src/session/factory.rs::production_blueprint`，装配在 `src/assembly.rs`。builtin handler/tool 位于 `../mcp-packages/`；本 crate 的 `src/mcp/builtin/` 保留策略、分派和运行时。


自动 compact 属于 Agent 执行阶段，参见 `../peri-agent/CLAUDE.md`。

## 数据流/架构

`SessionContext/config → Agent session factory（唯一触发点与链序蓝本）→ assembly 槽位构造 → MiddlewareChain → prompt/tools → Agent stage`。

- 插件提供 skill roots、agent dirs、hook groups 与 MCP 配置，由对应中间件消费。
- MCP 配置按数据面全局路径（默认 `~/.peri/settings.json`，尊重 `--config-file`）、插件、项目 `{cwd}/.mcp.json` 合并；配置正文、保存与来源身份经独立 `peri-mcp-config` MCP 通道，不依赖尚未启动的工具池。工具与资源仅在 pool 可用时注册。
- 宿主或全局配置可用 `mcpServers.workspace: { "url": "https://…/mcp" }` 接管内置 Workspace；项目/插件配置不可把同名远端标记为受信 Workspace。此远端的 `tools/list` 是 direct 工具全集的事实源（空列表有效），资源面也从该连接读取；`WorkspaceMiddleware: false` 只关闭内置实现，不关闭显式远端替代。连接协议由 MCP client 自动协商，不声明 `protocolVersion` 或 `system_mcp_tools`。
- 图片附件及归因的文件正文经当前会话可见的 builtin workspace MCP 读取；能力关闭、断连或换代后不回落宿主文件系统。hook loader 的正文与 canonical 来源去重经配置数据面，保留 symlink 同文件判定；身份不可得时跳过项目来源，不假定为独立来源。
- Skills 的**来源**（用户目录、项目目录、插件根、内置静态资产）由 builtin `workspace` 实例的资源面（`skills/list` + `resources/read`）提供；宿主只做根解析适配器（`src/skills/loader.rs::resolve_skill_roots`：路径 + scope/标签，不检查目录存在性），`src/settings.rs` 只读取 `disableBundledSkills`；`skillsDir` 配置链路已删除。扫描语义（叶子、深度/目录预算、symlink 口径、同名先到先得）在 provider（`mcp-packages/workspace/src/resources/skills.rs`）。
- SubAgent 从父工具、冻结上下文、取消策略与事件处理器派生执行上下文；具体 agent 定义和内置 agent 请直接查 `src/subagent/` 与项目 `.claude/agents/`，如需举例只使用 `explorer`。
- SubAgent 在父 Reason 目录发布时绑定当前 MCP 工具快照，避免装配早于 MCP 就绪而漏工具；子链收集自身工具并经 ToolSearch 发现、执行 deferred 工具。搜索与执行只消费子 Agent 过滤后的目录，`tools: []` 不得获得元工具。

## 任务路由

| 任务 | 首选位置 |
| --- | --- |
| 生产链顺序、条件注册、跨 crate 装配 | `../peri-agent/src/session/factory.rs`（蓝本）与 `src/assembly.rs`（槽位构造） |
| hook 状态能力与回写（ARC-MIDDLEWARE-CAPABILITY-001） | `../docs/standards/architecture-contracts.md`；`../peri-agent/src/middleware/capabilities.rs` |
| MCP 合并、server/tool bridge | `src/mcp/` |
| Plugin manifest、commands、agents、MCP 回退 | `src/plugin/` |
| Hook 事件与执行器 | `src/hooks/` |
| Skills 根解析、registry 投影、预载、工具（零 FS） | `src/skills/`（`loader.rs` 根解析 / `mod.rs` 投影与摘要 / `tools.rs` 两工具）、`src/settings.rs`、`src/subagent/skill_preload.rs` |
| SubAgent、后台任务、取消和事件 | `src/subagent/`；Agent 定义类型与纯解析在 `../mcp-packages/common/src/agent_definition/`，本地扫描在 workspace MCP 资源面 |
| HITL 权限与审批 | `src/hitl/` |
| Workflow、工具搜索 | `src/workflow/`、`src/tool_search/` |
| Builtin MCP handler、工具与公共映射 | `../mcp-packages/{common,web,artifact,cron,workspace}/`；宿主分派与生命周期在 `src/mcp/builtin/{context,dispatch,runtime}.rs` |
| 文件 / 终端工具与 Cron 调度 | `../mcp-packages/workspace/src/{filesystem,terminal}/`、`../mcp-packages/cron/`；Todo 仍在 `src/middleware/todo.rs` |

## 稳定不变量

- **链顺序**：只能在 Agent 层 session 工厂的链序蓝本（`production_blueprint`）与 `src/assembly.rs` 槽位构造中判断与修改生产顺序；不得按名称或局部便利重排。
- **MCP**：保留三层合并、内容去重和插件命名空间；配置来源或工具注册变更必须同时检查 pool、资源与 bridge 路径。init/OAuth/reconnect/subscription 任务由 deployment-held non-Clone `McpTaskOwner` 持有，并实现契约层 `McpTaskOwnerPort` 供 ACP boxed 注入；pool 只持 weak spawner。正常关闭顺序固定为 pool begin-close → owner abort/join → pool service close。Pool service close 由 pool-held 单一 transaction 持有，waiter 取消/并发/重试必须观察同一 `McpPoolShutdownReport`；cleanup timeout 保持 `Closing`，不得发布 `Closed`（ARC-HOST-SHUTDOWN-001）。
- **MCP 调用恢复**：仅 workspace builtin 保留原生期限，其他 builtin 与外部 MCP 发送/响应共用 120 秒。超时或 drop 发有界取消，handler 监听 request token；Workspace Bash 的后台完成证据归 Workspace MCP Task owner，Peri 通过 `tasks/get` 与 `subscriptions/listen` 消费。仅投影安全原因及工具生成的任务、日志、草稿引用，不透传任意错误。宿主入口见 `src/mcp/`，workspace 工具见 `../mcp-packages/workspace/`。blocking 搜索只协作取消，不承诺 drop 时已 join。
- **MCP over ACP**：client 在会话 setup 声明的 `type: "acp"` server 由 `src/mcp/acp/`（`AcpMcpService`）承载。`attach` 只登记并后台建连（会话建立不等连接），失败留在池状态面（`ConfigSource::Acp` 条目）而不回抛；连接按声明它的会话归属，工具桥接、发现面与状态面必须按 `is_visible_to_session` 过滤，不得跨会话泄漏；`mcp/message` 内层错误码原样透传（lifecycle 依赖方法级错误码）；会话结束在池关闭前 `close_session`（幂等，`mcp/disconnect` 有上界）（ARC-MCP-ACP-001）。
- **System MCP 启动准入**：`system_mcp=true` 的 server 须在首个 Reason 前完成 transport、initialize、能力协商与真实 `tools/list`（空数组成功，Err 不是发现证据），由 `before_react_start` 闸门阻断未就绪 loop。失败/timeout 返回类型化错误，不发布 ready；取消按中断分类。`system_mcp_tools` 按所属 server 原始工具名精确匹配；仅选中项 direct，未选中项维持原发现路径，`[]` 仅要求 ready。选中项以原始工具名进入模型面，普通 MCP 与 deferred 工具仍使用 `mcp__<server>__<tool>`。模型名冲突按确定准入顺序 first-wins，记录 warning 并跳过后续项；真实必需工具缺失仍失败。权限使用绑定来源身份，不凭裸名授予 builtin 权限。readiness 不绕过 Permission/HITL/事件/cancel。builtin 同构：web/artifact/workspace 选中各自 direct 集，cron 零提升。默认层注入，加载期拒绝 `disabled + system_mcp`。
- **插件 MCP 配置严格路径**：`load_enabled_plugins_for_mcp` 对非法 MCP 配置直接失败、不降级为空配置；宽容展示 API（`load_enabled_plugins_aggregated` 等）行为保持不变。
- **Plugin manifest**：`commands` 条目兼容字符串路径与对象；字符串是相对插件根目录的路径。agents 未声明时仍保留约定目录回退。不要把路径条目当作名称。
- **Skills（J5）**：宿主不做任何技能目录扫描或正文读取——目录来自会话级 `McpSkillRegistry` 投影（`before_agent`），正文只经统一 activation（`resources/read` + digest/frontmatter）。系统来源按连接身份识别（builtin 与受信 WorkspaceRemote），仅投影 `core:{skill}` 裸名命令；外部 MCP 仅投影 `{server}:{skill}`。`SkillsMiddleware` 关闭位由同一份 `disabled_middlewares` 派生，关闭后撤下系统命令并跳过主 Agent 的 slash token 自动预载；旧 `/server:skill` 系统 token 不再自动激活，子代理/workflow 的显式名单仍按原解析语义。根优先级/递归边界/叶子语义/同名覆盖是 **provider** 的契约；宿主只解析根列表（`resolve_skill_roots`）并与配置位一起作为 `WorkspaceResourcesInput` 注入。缺失即缺口（warn/空），**不回落磁盘**。
- **SubAgent**：同一会话的子 Agent 复用冻结指引、skills 与 prompt；同步子任务继承父取消，独立后台任务自管取消。`Agent(resume_thread_id, prompt)` 优先把非空 Info 投递给 live 执行（`action: send / status: queued`）；无接收者但磁盘仍 active 时拒绝，非 active 才 resume。Info 不打断模型，queued 不代表已读；事件按 `source_agent_id` 归属，改动需覆盖完成与取消路径。
- **HITL**：审批以解析后的 effective tool name 为准，包装、搜索或代理工具不得绕过审批；权限模式与 broker 的选择必须保持一致。
- **工具可见性**：direct/deferred 语义由工具声明和工具搜索路径共同保证，包装层不得改变其可见性。

## 目标命令

从仓库根目录执行：

```bash
./scripts/cargo-rmcp-patched.sh build --locked -p peri-middlewares
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::task_scope
./scripts/cargo-rmcp-patched.sh test --locked -p peri-middlewares --lib -- mcp::acp
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib
```

## 按需引用 / Verify

- 链、工具注册与条件中间件：`../peri-agent/src/session/factory.rs` 与 `src/assembly.rs`；同时遵守 `../docs/standards/architecture-contracts.md` 的 `ARC-MIDDLEWARE-001`、`ARC-TOOLS-001`、`ARC-FROZEN-001`。
- Plugin/MCP 或 Skills 改动：阅读目标模块的实现与测试后运行对应 package 与 host 测试；workspace host filters 为 `mcp::workspace_builtin_tests`、`mcp::workspace_recovery_tests`，Cron host 生命周期为 `mcp::builtin_cron_runtime_tests`。
- System MCP 准入 / 工具注入改动：`cargo test -p peri-middlewares --lib -- mcp::system_tools`、`-- mcp::client::readiness`、`-- mcp::middleware`；契约测试 `cargo test -p peri-middlewares --test mcp_host_policy_contract -- --test-threads=1` 与 `--test mcp_isolation_contract -- --test-threads=1`。
- SubAgent 或 HITL 改动：覆盖冻结数据、取消、事件归属及 effective tool name 的相关测试。
- 所有修改完成后运行 `git diff --check`；不得在日志、错误或测试 fixture 中写入密钥、token、密码或连接串。

## W5（2026-09-29，提交 f66bd251）

**W5 交付事实（2026-09-29，提交 f66bd251）**：① `McpAgentRegistry` 增会话可见性 + A24 关闭集 + host-assigned 来源（`AgentSource::{Local{scope,plugin},Remote}`，本地只认真实 builtin `workspace` 实例）；② builtin agent 资产迁 `mcp-packages/workspace/src/resources/builtin/agents/*.md`（`BUILTIN_AGENTS` 静态表，`agent://builtin/{id}/agent.md`），`peri-middlewares/src/subagent/built-in/*.md` 与 `built_in_agents.rs` 删除；③ 定义加载（`subagent/tool/definitions.rs`）与 workflow `resolve_agent_definition`（改 async，#[async_trait]）统一走 registry；④ `{{available_agents}}` 改由 `AgentCatalogPort::catalog`（`host_ports::AgentCatalogProvider` 绑定同一 registry，装配点 `preparation.rs` downcast）；⑤ 项目指令走 `peri-instruction://workspace/{main|local}`（P4 读取 → `FrozenInstructions` → 冻结构建），`AgentsMdMiddleware` 纯 adapter（无 `std::fs`），excludes 迁 provider 输入 `instruction_excludes`；⑥ 删除：`AgentDefineMiddleware` 模块与 `ChainSlot::AgentDefine`（蓝本槽位 21→20，AskUser/Permission 位置前移 1）、`scan_agents*`、`ReadDefinition`/candidate_paths、`SkillsPort`（→ `AgentCatalogPort`）、`plugin_agent_dirs`/`claude_md_excludes` 死管线。 证据：`cargo check --workspace --all-targets` 0 error/0 warning；`-p peri-mcp-workspace --lib` 368 passed；`-p peri-middlewares --lib subagent::tool` 107 passed；`mcp::agent_registry` 10 passed；`assembly::tests` 36 passed；fmt/layer-imports/diff-check 均 EXIT 0。未完成：验收缺口用例（关闭矩阵 E2E 已有 1 条、未知字段/ excludes 各 1 条）、全量 middlewares/acp 汇总行与 clippy（由协调者统一跑）、6 处文档的完整展开。

**W5 补正（协调者审查裁决后，2026-09-29）**：① **F2**：workflow 面显式拒绝 `mcp__*` 远端 id（该面没有批准 seam；`resolve_agent_definition_via_registry`）。② **F3**：远端 agent 的 `skills` 恢复清空（技能级批准面不存在 —— W2b 结论 ⇒ 声明不构成隐式授权）；本地受信来源仍保留（X3-A）。③ **F4**：恢复 `{cwd}/agents` 为第二个 project 根（顺序在 `.claude/agents` 之后，与迁移前候选序一致）；登记扩张：`{cwd}/agents/*` 现在也进 `{{available_agents}}` 目录（迁移前只可显式加载）——「可激活即可发现」。④ **F5**：`peri-acp/src/host/workspace.rs` 拆出 `workspace_resources.rs`（1012 → 910 + 111 行，STD-SIZE-001 达标）；`session_lifecycle.rs`(1544，HEAD 1531)、`session_data.rs`(1278)、`peri-agent/src/agent/stages/mod.rs`(1011) 为**存量**超限（`check-file-size.sh` 报源码 3 / 测试 29），非本波引入，另立任务。⑤ **F6**：agent 名按来源拆分校验——本地走契约段校验（`_`/大写历史名可用，拒绝时 warn）+ 非 `mcp__` 前缀，远端沿用 HEAD 严格集。⑥ **F8**：关闭/断连的宿主绑定 builtin `workspace` 句柄**整体跳过**，不再投影成 `mcp__workspace__*` 远端条目。⑦ **F11**：DiscoverMCP 的 agent 投影绑定会话与 `SubAgentMiddleware` 关闭位（装配点从同一份 `meta_harness_disabled` 派生，单一来源）。⑧ **F13**：同 id 两形态优先级（目录形态先）显式化并加断言。⑨ **F14 纠错**：`plugin_agent_dirs` 在迁移前是**活管线**（`assemble.rs` → `frozen.rs` 喂 `scan_agents_detailed` 与 SubAgent loader），本波改为 provider 插件根输入后参数整体退场——不是「死管线」。⑩ 登记：symlink agent 定义不再可发现（X3 存量失效面，与技能侧并列）；指令链槽关闭不门控 P4 读取（与 W4b 技能面同构，行为保留）；legacy 首次接纳无执行环境 ⇒ 指令与技能摘要均不可得（J2 §3.1，新增 warn 信号）；preload 缺口报告强度属 W6。

**W5 全量链与 flake 收口（协调者，2026-09-29）**：① acp 首轮全量抓到 1 例失败：`host::requests::tests::frozen_cases::test_session_load_cold_host_restores_original_frozen_prompt`（创建期 `claude_md()` 为 `None`）。根因经确定性实验钉死（`PERI_MCP_BUILTIN=off` 稳定复现）：`BuiltinInjectionPolicy::from_env()`（`peri-middlewares/src/mcp/config.rs`）在配置加载期读**进程级 env**，开关组用例在 `#[serial]` 临界区内置 `off`（`mcp_v4_builtin_test.rs:81`、`mcp_v4_wave2_test.rs:731`、`mcp_v4_startup_test.rs:142`），而 `serial_test` 只互斥标注者——读侧 4 个用例漏标 ⇒ 并行窗口内池无 `workspace` 句柄且 `initPhase` 已收口 `ready` ⇒ 资源面按 X4 静默缺席（**生产语义正确，缺陷在测试隔离**；owner 复跑另打中 `prepared_tests::new_session_persists_frozen_bytes_from_its_single_preparation`，同一形状）。修复：`prepared_test.rs` ×2、`requests_frozen_cases_test.rs` ×1、`requests_workspace_cases_test.rs` ×1 补 `#[serial]`（`TEST-HERMETIC-001` 读侧闭合；零断言放宽、零生产代码改动、未改 10s 上界；套件 74.4s → 77.4s）。此前记「已知 flake」的 `workspace_cases::worktree_new_resources_use_the_target_directory` 同根因、同批修复（升为已定缺陷）。② 最终链：`-p peri-acp --lib` **775 passed / 0 failed**（EXIT=0）；`-p peri-mcp-workspace --lib` **372 passed / 0 failed / 1 ignored**（EXIT=0）；`clippy --workspace --all-targets -- -D warnings`、`fmt --all --check`、`check-layer-imports.sh`（20 规则 / 0 违规）、`git diff --check` 全 EXIT=0；`check-file-size.sh` EXIT=1（源码 3 / 测试 29，均存量）。③ 存量负载敏感 flake 登记（非本波引入，未修复）：`mcp::workspace_recovery_tests::external_source_keeps_120_second_deadline_and_cancels_execution` —— `-p peri-middlewares --lib` 首轮 1539 passed / 1 failed（`assert_process_gone` 5s 上界超时，`workspace_recovery_test.rs:157`；文件自 `fb54f614` 起未改动、用例自身构造 ≥120s），首轮失败证据保留；单测复跑 1 passed（120.0s）、全量复跑 **1540 passed / 0 failed / 4 ignored**（EXIT=0）。④ 约定（`peri-acp/CLAUDE.md` Verify）：触碰 builtin `workspace` 资源面的用例必须与进程级开关组同键 `#[serial]`。
