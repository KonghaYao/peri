# Peri 框架架构审查报告

**审查范围**：9 个 workspace crate（peri-agent / peri-middlewares / peri-widgets / peri-acp / peri-tui / langfuse-client / peri-lsp / agm / side-projects/git-graph）
**审查维度**：分层架构、职责单一性、抽象质量、错误处理、状态管理、并发设计、测试质量、设计模式
**审查方法**：多 agent workflow —— 9 个 Explore agent 并行扫描 crate → 8 个 code-reviewer agent 多维度评审 → opus agent 对抗性验证每条 high/medium 发现 → opus agent 综合报告
**报告日期**：2026-06-14

---

## 一、总体评分与一句话总结

**总体评分：7.0 / 10**

整体水平处于**资深工程师主导的中大型 Rust 项目**层次——核心抽象（Middleware trait、LLM Decorator 栈、AcpTransport 策略、Frozen data pattern）设计成熟，陷阱文档（CLAUDE.md [TRAP] 体系）对历史踩坑有详尽固化；但在快速迭代中累积了若干结构性债务，最显著的是 TUI 层与 peri-acp 层在会话管理上形成平行实现，以及 `execute_prompt` / `AcpAgentConfig` 两个"上帝签名/结构"成为高频变更热点。

各维度分布：

| 维度 | 分数 | 评价 |
|------|------|------|
| 分层架构清晰度 | 6.5 | Cargo 依赖图严格无环，但 TUI acp_server 实质绕过 peri-acp 的 SessionManager |
| 职责单一性 | 5.0 | 多个 God struct（AcpAgentConfig 34 字段、AgentComm 30 字段）+ 超长函数（execute_prompt 533 行） |
| 抽象质量 | 7.5 | trait 分层教科书级，但存在多处"接口已定义但实现单一"的过度抽象 |
| 错误处理 | 6.5 | thiserror/anyhow 边界清晰，但中间件/TUI 层大面积 String 化 |
| 状态管理 | 7.0 | Frozen pattern 设计精良，但仅靠约定保证不可变 |
| 并发设计 | 7.5 | deferred_error + try_break! + biased cancel 是最佳实践，少量调试残留 |
| 测试设计 | 7.0 | 186 个 _test.rs 文件组织规范，但 rewind 零覆盖是显著缺口 |
| 设计模式 | 7.0 | Builder/Decorator/Strategy 应用成熟，缺 Type State 保护非法状态 |

---

## 二、架构强项（值得保持的设计选择）

### 1. Cargo 依赖图严格单向无环

`peri-widgets` / `peri-lsp` / `langfuse-client` 三个基础库零 workspace 内部依赖；`peri-agent` 仅依赖外部 crate；`peri-middlewares` → `peri-agent` + `peri-lsp`；`peri-acp` → `agent`/`middlewares`/`lsp`/`langfuse`；`peri-tui` 在顶层。workspace resolver = "2" 禁止下层依赖上层，从编译期杜绝了循环依赖。

### 2. LLM 适配层的 Adapter + Decorator 三层栈

```
BaseModel (底层 LLM trait)
   ↓ BaseModelReactLLM (Adapter：桥接 BaseModel → ReactLLM)
ReactLLM (高层 ReAct trait)
   ↓ RetryableLLM<L: ReactLLM> (Decorator：泛型装饰器叠加重试)
ReactLLM (带重试)
```

- `RetryableLLM<L>` 使用**泛型参数**而非 `Box<dyn ReactLLM>`，编译期确定装饰目标，零运行时开销
- 每层职责单一、可独立测试，新增 provider 只需实现 `BaseModel`，上层自动获得重试能力
- 这是项目中最成熟的模式组合，位于 `peri-agent/src/llm/react_adapter.rs:114-210` 与 `retry.rs:63`

### 3. Middleware<S: State> trait 的 9 个生命周期钩子

`peri-agent/src/middleware/trait.rs:27-117` 定义了 `before_agent` / `before_model` / `after_model` / `before_tool` / `before_tools_batch` / `after_tool` / `on_error` / `after_agent` / `name` 共 9 个钩子，每个都有默认 no-op 实现。`before_tools_batch` 提供批量优化路径覆盖默认逐条实现。18 个生产中间件（CompactMiddleware、HitlMiddleware、HooksMiddleware、SubAgentMiddleware 等）+ 大量测试中间件证实了这个抽象的扩展性。

### 4. deferred_error + try_break! 的并发错误处理

`tool_dispatch.rs` 的两阶段写入模式（`collect_tool_results` 不写 state → `dispatch_tools` 统一写入）有效防止并发工具执行过程中 state 不一致。P3/P4 错误路径用 deferred_error 模式收集所有错误后统一判断，确保所有 tool_result 始终写入 state。`try_break!` 宏（`executor/mod.rs:285-295`）将 `?` 传播转为 loop-break + post-loop cleanup，确保 `cleanup_prepended` 无论成功/失败/循环耗尽都执行——这是 Rust 异步控制流中的最佳实践。

### 5. Frozen data pattern（会话内不可变数据）

`FrozenSessionData`（`executor.rs:62-84`，7 字段）在 `session/new` 时一次性捕获，通过 `build_frozen_session_data()` 构造后传入每轮 `execute_prompt`，保证 prompt cache 前缀稳定。全代码库 grep `frozen.*mut` 返回零结果，约定不可变性被严格遵守。配合 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 边界标记分隔静态/动态区域，是 prompt cache 稳定性的工程化保障。

### 6. AcpTransport + EventSink 双策略隔离传输差异

`AcpTransport` trait 4 个方法覆盖 JSON-RPC 2.0 双向通信全部语义，`MpscTransport`（in-memory）与 `StdioTransport`（IDE）结构高度对称。`EventSink` trait 统一 `TransportEventSink`（TUI）与 `StdioEventSink`（IDE），共享同一 `executor::execute_prompt`，避免业务逻辑重复。扩展 WebSocket 传输只需新增一个 trait 实现。

### 7. 有序 mpsc channel 持久化替代 fire-and-forget spawn

`AgentState` 持久化通道（`state.rs:131-141`）使用 `mpsc::unbounded_channel` + 专用 writer task，从根本上解决了 `.await` 让步导致的写入乱序问题。相比 `tokio::spawn` 的 fire-and-forget，保证了消息落盘顺序与内存顺序一致。

### 8. CLAUDE.md [TRAP] 体系沉淀历史踩坑

20+ 条 [TRAP] 注释覆盖了系统提示词稳定性、流式文本可见性、孤立 tool_use、cleanup_prepended 泄漏、SubAgent cache 隔离等历史 bug，每条都关联到 `spec/global/domains/` 下的详细 issue 文档。这是项目最宝贵的隐性资产，新贡献者通过阅读 [TRAP] 即可避开已知陷阱。

---

## 三、主要弱点（按优先级排序，Top 10 真实问题）

### 弱点 1：TUI acp_server 是 peri-acp SessionManager 的平行实现（P0 级架构债务）

**位置**：`peri-tui/src/acp_server/mod.rs:34-47`（SessionState）vs `peri-acp/src/session/mod.rs:37-56`（AcpSession）

**问题**：TUI 自行定义了 `SessionState`（7 字段：session_id/thread_id/cwd/history/cancel_token/frozen/agent_pool），而 peri-acp 已定义了功能等价的 `AcpSession`（12 字段）。两者的 frozen 数据构建逻辑重复出现在 5 处：

- `peri-tui/src/acp_server/requests.rs:81`（session/new）
- `peri-tui/src/acp_server/requests.rs:255`（session/load）
- `peri-tui/src/acp_server/requests.rs:371`（session/resume）
- `peri-tui/src/acp_server/requests.rs:424`（session/fork）
- `peri-tui/src/acp_stdio/freeze.rs:16`（stdio 路径）

**铁证**：`prompt.rs:133` 与 `prompt_exec.rs:88` 均显式传 `None, // session_manager`，意味着 peri-acp 的 `SessionManager` **从未被实例化**。

**正在发生的坍塌**：commit `9d74169b`（2026-06-12，2 天前）给 `AcpSession` 新增了 `goal_state` 字段，但 TUI 的 `SessionState` 和 stdio 的 `SessionInfo` 均未同步——"双重维护"陷阱已在现实中发生。

**副作用**：由于 `session_manager=None`，executor.rs 中依赖它的 cascade cancel 子 agent 逻辑（L452-475 `register_runtime`/`deregister_runtime`，L617-622 `cancel_cascade_children`）在 TUI/stdio 路径下**完全失效**。

**建议模式**：Shared Kernel / Delegation Pattern
1. 将 `SessionState`（TUI）和 `SessionInfo`（stdio）合并为对 `AcpSession` 的引用
2. 将 5 处 `build_frozen_session_data` 调用提取到 `SessionManager::new_session_with_id/load/resume/fork` 方法内
3. 将 `prompt.rs:133` 和 `prompt_exec.rs:88` 的 `None` 替换为实际 `SessionManager` 实例，恢复 cascade cancel 与 goal_state 功能
4. 补全 goal_state 到 TUI/stdio 路径

**工作量**：L（1-2 周）

---

### 弱点 2：AcpAgentConfig 34 字段 God Struct + build_agent 440 行（P1）

**位置**：`peri-acp/src/agent/builder.rs:52-103`（struct）、`:124-563`（build_agent 函数）

**问题**：`AcpAgentConfig` 实际有 34 个 pub 字段（声称 30，低估 4 个），覆盖 LLM provider/model、frozen 会话数据（4 字段）、事件处理、HITL broker、Cron、Hook（2 字段）、MCP、Channel、tool_search、LSP、Compact（4 字段）、线程持久化（4 字段）、SubAgent 等异质关注点。`build_agent` 函数体 440 行，内部含 18 次 `.add_middleware()` 调用。调用方 `executor.rs:480-523` 用 ~43 行逐一填充字段。

**影响**：git log 显示该文件近 20 次 commit 持续被修改，是高频变更热点。

**注意**：18 个中间件的固定顺序是 [TRAP] 守护的契约，不可拆分；且 `all_hooks`（L441 收集、L539 消费）、`bg_event_tx`（L338 创建、L356 传给 SubAgentMiddleware、最终返回）存在跨字段生命周期依赖，盲目拆分会割裂可见性。

**建议模式**：渐进式子配置分组（Facade + Builder），**不要一次性拆分全部 34 字段**
- 第一阶段（最高收益最低风险）：抽取 `FrozenData { claude_md, claude_local_md, skill_summary, date }`、`CompactSettings { config, budget, model, event_tx }`、`ThreadPersistence { store, parent_thread_id, register_runtime, deregister_runtime }` 三组共 12 字段（零跨依赖）
- 第二阶段：Hook/MCP/LSP/Cron 保持平铺（有跨字段依赖），仅添加文档注释标注分组
- 保留 `build_agent` 单体函数（中间件顺序是 [TRAP] 守护的契约，分散会降低顺序可审计性）

**工作量**：M

---

### 弱点 3：execute_prompt 533 行 + 30 参数（P1）

**位置**：`peri-acp/src/session/executor.rs:102-634`

**问题**：函数体 533 行（声称 706 行含签名和空行），30 个参数（声称 20+ 低估），`#[allow(clippy::too_many_arguments)]` 抑制警告。9 个阶段线性编排：bg_results 注入 → compact 配置 → bg cmd 通道 → 命令拦截 → agent_input → event pump spawn → build_agent → bg event pump spawn → todo 转发 spawn → agent 执行 → 结果处理。

**影响**：无可测试性——`peri-acp/src/session/` 下无 `executor_test.rs`，全 crate 无任何测试引用 `execute_prompt`。3 个调用点（`prompt.rs:106`、`cli_print.rs:196`、`prompt_exec.rs:61`）必须传齐全部 30 参数，`cli_print.rs:196-225` 传了 6 个 `None`/`vec![]` 占位符并配注释说明——位置参数脆弱性的铁证。

**建议模式**：Parameter Object + Extract Method
1. 引入 `PromptExecutionContext` 结构体，字段分组：
   - `session_context`：cwd/session_id/frozen/history/incoming_recalls/is_empty_history/thread_store/thread_id/session_manager/bg_results
   - `agent_config`：permission_mode/plugin_skill_dirs/plugin_agent_dirs/hook_groups/cron_scheduler/mcp_pool/channel_state/tool_search_index/shared_tools/lsp_servers
   - `telemetry_config`：langfuse_session/event_sink/pool
   - 顶层：provider/content/cancel/broker
2. 拆为 4 个私有方法：`intercept_immediate_command`、`spawn_event_pump`、`build_and_execute_agent`、`collect_result`
3. `execute_prompt` 保留为编排器
4. 补一个 `executor_test.rs` 覆盖 `intercept_immediate_command` 分支

**工作量**：M

---

### 弱点 4：TUI acp_server Prediction 功能直接构建 ReActAgent（P1，违反 [TRAP]）

**位置**：`peri-tui/src/acp_server/mod.rs:205-218`

**问题**：在 TUI 层直接调用 `BaseModelReactLLM::new()`、`RetryableLLM::new()`、`ReActAgent::new().max_iterations(1).with_system_prompt(directive)` 构建 agent，并在 L230 调用 `agent.execute()`。违反 CLAUDE.md 明确标注的 [TRAP]："Agent 构建和执行统一通过 `peri_acp::session::executor::execute_prompt()`。禁止在 TUI 层直接构建 ReActAgent"。

**缓解因素**：这是一个有意的、最小化的 Fork 风格路径（1 次迭代、无工具、无中间件、无 HITL、无 compact、无 frozen-system-prompt 涉及），不存在缓存不稳定或安全性风险。同模块常规 prompt 路径（`prompt.rs:106`）正确委派给 executor，证明作者知道正确模式。

**建议模式**：Facade Pattern
1. 在 `peri-acp/src/session/executor.rs` 新增 `pub async fn execute_prediction(provider, history, cwd) -> Result<String, _>`
2. 将 `mod.rs:205-218` 的构造逻辑和 30 秒超时调用（L228-238）移入其中
3. TUI 的 prediction 分支改为 `let text = peri_acp::session::executor::execute_prediction(...).await`，保留现有 `peri/prediction_ready` 通知发送（L267-275）

**工作量**：S（半天）

---

### 弱点 5：rewind 命令零测试覆盖——最复杂的 slash command 完全未测试（P1）

**位置**：`peri-acp/src/session/command/rewind.rs`（345 行）

**问题**：`grep -c 'fn test_|#[test]|#[tokio::test]' rewind.rs` 返回 0。该命令包含 5 个高风险步骤：消息截断（`rewind.rs:113-114` 的 `history[target_idx..]` 独占边界）、FileChange 提取（`extract_file_changes` L167）、逆向文件恢复（`revert_files` L239，执行 `std::fs::write`/`remove_file` + `git checkout HEAD --`）、ToolUse/ToolResult 配对验证（`validate_tool_pairing` L303）、SQLite 持久化删除。同级 `bg_test.rs` 有 10 个测试，`compact.rs` 有 28 个内联测试，rewind 是唯一零覆盖的 Immediate 命令。

**潜在 bug**：`revert_files` 的 Edit 分支（L252-258）使用 `content.find(new_string)` + 字节切片 `&content[..idx]`，如果 `new_string` 跨越 UTF-8 字符边界，**会 panic**。

**建议模式**：Strategy Pattern + Test Data Builder
1. 创建 `rewind_test.rs`，注册到 `mod.rs` 的 `#[cfg(test)]`
2. 用 `tempfile::tempdir()` 作为 cwd 测试 `revert_files` 的 Write/Edit 分支
3. 构造未配对的 ToolUse/ToolResult 消息测试 `validate_tool_pairing`
4. 用 `MockEventSink` 测试 `execute` 的未找到目标、末尾截断、中间截断三种场景
5. **优先修复** `revert_files` 的 UTF-8 边界 bug（改用 `content.char_indices().nth(idx)`）

**工作量**：M

---

### 弱点 6：AgentComm 30 字段混合 6 个不相关关注点（P2）

**位置**：`peri-tui/src/app/agent_comm.rs:26-87`

**问题**：30 个 pub 字段混合：(1) ACP 通信（acp_notification_rx/pending_acp_request_id）、(2) HITL/AskUser 交互（interaction_prompt/pending_hitl_items/pending_ask_user）、(3) 消息历史（origin_messages）、(4) Token/重试指标（session_token_tracker/context_window/retry_status/tool_call_count）、(5) SubAgent/后台任务（subagent_depth/pending_bg_continuation/agent_done_pending_bg/pre_done_bg_*）、(6) LSP 诊断（lsp_errors/warnings/files_with_errors）。23 个文件通过 `current_mut().agent.<field>` 访问共 134 次。`agent_submit.rs:123-144` 的提交重置链横扫这些字段。

**历史 bug 证据**：`cancel_token` 的 `Option` 语义歧义（None 表示"无活跃任务"还是"TUI+ACP 路径从不创建"）曾导致 `2026-05-24-cancel-ineffective-during-streaming-and-tool-execution.md` 记录的实际 bug。

**建议模式**：Value Object Composition，**不要一次拆完 5 组**
- 第一步（ROI 最高）：抽取 `BgTaskState { pending_bg_continuation, agent_done_pending_bg, pre_done_bg_completions, pre_done_bg_results }`，新增 `reset_for_new_round()` 方法替换散落重置语句
- 第二步：抽取 `LspDiagnostics { errors, warnings, files_with_errors }`（总是一起重置）
- 不要机械执行原建议的全部 5 组拆分（会改动 134 处访问点而收益有限）

**工作量**：M

---

### 弱点 7：中间件层错误大面积 String 化，丢失结构化信息（P2）

**位置**：
- `peri-middlewares/src/cron/mod.rs:50` 返回 `Result<String, String>`
- `peri-middlewares/src/cron/tools.rs:50` 返回 `Result<String, Box<dyn Error + Send + Sync>>`
- `peri-middlewares/src/background.rs:52,100` 返回 `Result<(), String>`
- `tool_dispatch.rs:314-318` 通过 `e.to_string()` 吞掉错误类型

**问题**：`BaseTool::invoke` 硬编码返回 `Result<String, Box<dyn std::error::Error + Send + Sync>>`（`peri-agent/src/tools/mod.rs:31-34`），所有工具实现适配到该类型。内部 API（CronScheduler::register、background register/cancel）用 String 做错误类型。边界处用 `e.into()` 或 `map_err(format!)` 转换。`AgentError::ToolExecutionFailed { reason: String }` 在 `tool_dispatch.rs:314` 通过 `.to_string()` 压平为字符串，source chain 在此切断。

**缓解因素**：工具错误在 ReAct 循环中是非终止的（错误 ToolResult 收集后由 LLM 下一轮修正），无任何调用方通过类型 match 区分这些错误。

**建议模式**：分两步推进
1. **短期**：为内部 API 引入 thiserror 枚举（参考已有的 `lsp/tool.rs:11-28` `LspToolError` 模式）——`CronError { InvalidExpression, TaskLimitReached, TaskNotFound }`、`BackgroundRegistryError { ConcurrentLimit, TaskNotFound }`
2. **长期**：为 `BaseTool` trait 增加关联类型 `type Error: std::error::Error + Send + Sync`（默认 `Box<dyn Error>`），`AgentError::ToolExecutionFailed` 增设 `#[source] source` 字段保留原始错误链

**工作量**：M

---

### 弱点 8：CompactConfig 的 11 个 default_xxx 函数与 Default impl 重复（P2）

**位置**：`peri-agent/src/agent/compact/config.rs`

**问题**：为 serde 默认值定义了 11 个独立函数（`default_true`/`default_threshold_085` 等），而 `Default` impl 中重复定义了完全相同的默认值，两处必须手动保持同步。

**建议**：统一为单一来源——保留 serde 函数，`Default` impl 调用这些函数；或用 `serde_with` 的 `#[serde(default)]` 配合 `Default` 派生。

**工作量**：S

---

### 弱点 9：tool_dispatch.rs [DEADLOCK] 调试残留 + metrics 空值（P3）

**位置**：`peri-agent/src/agent/executor/tool_dispatch.rs:109, 128, 173, 178, 187, 405, 419`（7 处 [DEADLOCK] 日志）、`:387`（metrics `"duration_ms": ()` 序列化为 JSON null）

**问题**：7 处 `tracing::debug!`/`warn!` 使用 `[DEADLOCK]` 前缀是死锁调查遗留的诊断代码；L405/L419 有 `if modified_call.name == "Agent"` 的 SubAgent 特定调试。L387 的 `duration_ms: ()` 因工具并发 join_all 无法获取单工具耗时，序列化为 null。

**建议**：
1. 删除 6 处 `[DEADLOCK] debug!`（L108-116、L127-130、L187-190、L403-408、L417-422），L173/L178 的 `warn!` 去掉前缀保留
2. metrics 要么在并发闭包内记录 `Instant::now()` 携带 duration，要么移除 `duration_ms` 字段直到正确接线
3. 验证 Langfuse/Grafana 面板是否依赖该字段

**工作量**：S

---

### 弱点 10：setup_wizard_test.rs 依赖真实文件系统路径（P3）

**位置**：`peri-tui/src/app/setup_wizard/setup_wizard_test.rs:64-72`

**问题**：`test_migrate_from_claude_code_no_file` 调用 `migrate_from_claude_code()`，内部使用 `dirs_next::home_dir()` 读取真实 `~/.claude/settings.json`，测试结果取决于运行环境。`test_migrate_syncs_all_fields`（L74-130）在注释中承认无法 mock，转而只测试辅助函数 `env_get`，核心迁移逻辑完全未覆盖。

**建议模式**：Constructor Injection
1. 在 `SetupWizardPanel`（`mod.rs:232`）添加 `home_dir_override: Option<PathBuf>` 字段
2. `migrate_from_claude_code()` 改为 `self.home_dir_override.clone().or_else(dirs_next::home_dir).unwrap_or_else(|| PathBuf::from("."))`
3. 重写测试用 TempDir 隔离，断言无文件返回 false、有有效配置返回 true 且 providers 正确填充

**工作量**：S

---

## 四、设计模式优化建议（重点章节）

### 当前模式分析

**中间件链**：`MiddlewareChain<S>` 内部 `Vec<Box<dyn Middleware<S>>>` 动态分发，热路径每轮调用 `before_model`/`after_model`/`before_tool`/`after_tool` 共 4 次 × 18 个中间件 = 72 次虚函数调用。不是真正的 Chain of Responsibility（无中断/短路机制）。

**装饰器栈**：LLM 适配层三层装饰是教科书级实现，无需改动。

**命令分发**：`AgentCommand` trait + `CommandKind`（Immediate/Passthrough/Transform）+ `CommandRegistry`，符合开闭原则。`rewind.rs` 的 5 个步骤是纯函数但未提取。

**传输策略**：`AcpTransport` 4 方法 + `EventSink` trait，易扩展。

---

### 建议 1：Parameter Object 模式消除 execute_prompt 参数爆炸

**现状**：`execute_prompt` 30 个位置参数，3 个调用点各传 6 个 `None`/`vec![]` 占位符并配注释。

**建议模式**：Parameter Object + 分阶段 Extract Method

**重构草图**（`peri-acp/src/session/executor.rs`）：

```rust
// 新增结构体
pub struct PromptExecutionContext {
    // 会话上下文（frozen/immutable 部分）
    pub session: SessionContext,
    // 中间件 provision（跨层穿透部分）
    pub middleware: MiddlewareProvision,
    // I/O 资源（per-turn mutable）
    pub resources: ExecutionResources,
}

pub struct SessionContext {
    pub cwd: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    pub frozen: Option<FrozenSessionData>,
    pub history: Vec<BaseMessage>,
    pub incoming_recalls: Vec<String>,
    pub is_empty_history: bool,
    pub bg_results: Vec<BackgroundTaskResult>,
}

pub struct MiddlewareProvision {
    pub permission_mode: Arc<SharedPermissionMode>,
    pub plugin_skill_dirs: Vec<PathBuf>,
    pub plugin_agent_dirs: Vec<PathBuf>,
    pub hook_groups: Vec<Vec<RegisteredHook>>,
    pub cron_scheduler: Option<Arc<Mutex<CronScheduler>>>,
    pub mcp_pool: Option<Arc<McpClientPool>>,
    pub channel_state: Option<Arc<ChannelState>>,
    pub tool_search_index: Arc<ToolSearchIndex>,
    pub shared_tools: Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>,
    pub lsp_servers: Vec<LspServerConfig>,
}

pub struct ExecutionResources {
    pub event_sink: Arc<dyn EventSink>,
    pub cancel: AgentCancellationToken,
    pub broker: Arc<dyn UserInteractionBroker>,
    pub pool: Arc<Mutex<AgentPool>>,
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    pub langfuse_session: Option<Arc<LangfuseSession>>,
    pub session_manager: Option<SessionManager>,
}

// execute_prompt 签名从 30 参数降为 4
pub async fn execute_prompt(
    ctx: &mut PromptExecutionContext,
    provider: &LlmProvider,
    peri_config: Arc<PeriConfig>,
    content: MessageContent,
) -> PromptResult {
    // 阶段 1：命令拦截
    if let Some(result) = intercept_immediate_command(ctx).await {
        return result;
    }
    // 阶段 2：启动事件泵
    let pump_done_rx = spawn_event_pump(ctx);
    // 阶段 3：构建并执行 agent
    let agent_state = build_and_execute_agent(ctx, provider, peri_config, content).await;
    // 阶段 4：收集结果
    collect_result(ctx, agent_state, pump_done_rx)
}
```

**收益**：
- 3 个调用点的占位符 `None`/`vec![]` 集中到 `MiddlewareProvision::default()` 或 builder
- 新增中间件参数只需在 `MiddlewareProvision` 加字段 + builder 方法，调用点零改动
- 4 个子方法可独立单元测试（补 `executor_test.rs`）
- 移除 `#[allow(clippy::too_many_arguments)]`

**工作量**：M（2-3 天）

---

### 建议 2：Delegation Pattern 统一会话管理到 peri-acp SessionManager

**现状**：TUI 的 `SessionState`（7 字段）、stdio 的 `SessionInfo`、peri-acp 的 `AcpSession`（12 字段）三者平行，frozen 构建重复 5 处，cascade cancel 因 `session_manager=None` 失效。

**建议模式**：Delegation + Shared Kernel

**重构草图**：

```rust
// peri-acp/src/session/mod.rs —— 扩展 SessionManager
impl SessionManager {
    pub async fn new_session_with_id(...) -> Result<(String, FrozenSessionData), AcpError> {
        // 内部统一构建 frozen，消除 TUI/stdio 各自实现
        let frozen = build_frozen_session_data(...).await;
        let session = AcpSession::new(...);
        self.sessions.insert(session_id.clone(), session);
        Ok((session_id, frozen))
    }
    pub async fn load(...) -> Result<(String, FrozenSessionData), AcpError> { ... }
    pub async fn resume(...) -> Result<(String, FrozenSessionData), AcpError> { ... }
    pub async fn fork(...) -> Result<(String, FrozenSessionData), AcpError> { ... }
}

// peri-tui/src/acp_server/mod.rs —— TUI 委托给 SessionManager
pub fn run_acp_server(transport: Arc<dyn AcpTransport>, session_manager: SessionManager) {
    // 不再自行维护 SharedSessions，所有会话操作通过 session_manager
}

// peri-tui/src/acp_server/prompt.rs:133 —— 替换 None
let session_manager = Some(sessions.clone()); // 不再是 None
peri_acp::session::executor::execute_prompt(..., session_manager, ...).await
```

**收益**：
- 消除 5 处 frozen 构建重复
- 恢复 cascade cancel 子 agent 功能（L452-475/L617-622）
- goal_state 等新字段自动通过 `AcpSession` 传播，无需下游同步
- commit `9d74169b` 的 goal_state 字段立即生效

**工作量**：L（1-2 周，需迁移 TUI 的 `SharedSessions` 和 stdio 的 `SessionInfo`）

---

### 建议 3：Facade Pattern 封装 Prediction 功能

**现状**：`peri-tui/src/acp_server/mod.rs:205-218` 直接构建 ReActAgent。

**建议模式**：Facade

**重构草图**：

```rust
// peri-acp/src/session/executor.rs —— 新增
pub async fn execute_prediction(
    provider: &LlmProvider,
    history: &[BaseMessage],
    cwd: &str,
) -> Result<String, AgentError> {
    let base_llm = BaseModelReactLLM::new(provider.into_model());
    let llm = RetryableLLM::new(base_llm, RetryConfig::default());
    let directive = build_prediction_directive();
    let agent = ReActAgent::new(llm)
        .max_iterations(1)
        .with_system_prompt(directive);
    let mut state = AgentState::new(cwd);
    for msg in history { state.add_message(msg.clone()); }
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        agent.execute(AgentInput::text("请根据以上对话预测用户下一步输入"), &mut state, None),
    ).await;
    // 提取最后一条 AI 消息文本
    ...
}

// peri-tui/src/acp_server/mod.rs:205-218 —— 替换为单行调用
let text = peri_acp::session::executor::execute_prediction(
    &pred_provider.read().clone(),
    &recent,
    &cwd,
).await.unwrap_or_default();
```

**收益**：TUI 层不再接触 `ReActAgent`/`BaseModelReactLLM`/`RetryableLLM` 内部类型，遵守 [TRAP] 约定。

**工作量**：S（半天）

---

### 建议 4：Test Data Builder 模式消除 tool_dispatch_test.rs Mock 重复

**现状**：`EchoTool` 结构体 + `BaseTool` impl 重复 4 份（L75-95、L196-216、L439-459、L917-937），`ThreeToolLLM` 重复 3 份（L97-122、L169-194、L412-437），合计 ~200 行纯重复。

**建议模式**：Test Fixture Factory（模块级共享 Mock）

**重构草图**（`peri-agent/src/agent/executor/tool_dispatch_test.rs` 顶部）：

```rust
// 文件作用域共享 Mock（CLAUDE.md 的"不跨文件共享"约束不阻碍文件内共享）
struct EchoTool { name_str: &'static str }
impl BaseTool for EchoTool { /* 来自 L75-95 的实现 */ }

struct ThreeToolLLM { final_answer: &'static str }
impl ReactLLM for ThreeToolLLM {
    // 来自 L97-122，把硬编码的 'all results processed' 替换为 self.final_answer
}

// 工厂函数（符合 make_ 命名规范）
fn make_echo_tool(name: &'static str) -> Arc<dyn BaseTool> { Arc::new(EchoTool { name_str: name }) }
fn make_three_tool_llm(answer: &'static str) -> ThreeToolLLM { ThreeToolLLM { final_answer: answer } }

// 各测试函数改为
let tool_a = make_echo_tool("EchoA");
let llm = make_three_tool_llm("all results processed");
```

**收益**：消除 ~150-200 行重复，测试语义零变化，新增字段时只需改 1 处 Mock 定义。

**工作量**：S（半天）

---

### 建议 5：Strategy + Immutable Value Object 模式强化 FrozenSessionData 类型安全

**现状**：`FrozenSessionData`（`executor.rs:62-84`）7 字段全 pub，仅 derive Clone，无类型系统保护。

**建议模式**：Immutable Value Object（封装 + accessor）

**重构草图**：

```rust
// peri-acp/src/session/executor.rs
#[derive(Clone)]
pub struct FrozenSessionData {
    // 字段改为 pub(crate)，外部只能通过构造函数和 accessor 访问
    pub(crate) system_prompt: Arc<str>,  // Arc<str> 替代 String，clone 零成本
    pub(crate) claude_md: Arc<str>,
    pub(crate) claude_local_md: Arc<str>,
    pub(crate) skill_summary: Arc<str>,
    pub(crate) date: Arc<str>,
    pub(crate) is_git_repo: bool,
    pub(crate) language: Arc<str>,
}

impl FrozenSessionData {
    /// 唯一构造路径，封装现有的 build_frozen_session_data() 逻辑
    pub fn build(...) -> Self { ... }
    pub fn system_prompt(&self) -> &str { &self.system_prompt }
    pub fn claude_md(&self) -> &str { &self.claude_md }
    // ... 其余 accessor
}
```

**收益**：
- 编译期禁止结构体外部 `frozen.system_prompt = ...`
- `Arc<str>` 让每轮 clone（必然发生）零成本
- 单一构造收敛点

**注意**：不要在 `AcpSession` 中添加第三个存储点——会偏离同步。

**工作量**：S（1-2 小时，机械重构）

---

### 建议 6：Single Source of Truth 模式消除双 config 所有权

**现状**：`ServiceRegistry.peri_config: Option<PeriConfig>`（`service_registry.rs:63`）与 `AcpServerConfig.peri_config: Arc<RwLock<PeriConfig>>`（`acp_server/mod.rs:54`）是两个独立存储，`main.rs:709-711` 用全新 Arc 构建，写入前者无法传播到后者。

**已确认的损坏点**：
- `command/session/lang.rs:47-50` 写入 `cfg.config.language` 并持久化，但从不调用 `sync_acp_config()`
- `app/config_panel.rs:88-98` 的 `save_config_now` 修改 compact 阈值（CLAUDE.md 记录每轮从 ACP 端重新读取）、persona、tone 等，从不调用 `sync_acp_config()`

**建议模式**：Single Source of Truth（共享 Arc）

**重构草图**：

```rust
// peri-tui/src/app/service_registry.rs:63
pub struct ServiceRegistry {
    // 改为共享 Arc，与 AcpServerConfig 共享同一实例
    pub peri_config: Arc<parking_lot::RwLock<PeriConfig>>,
    ...
}

// peri-tui/src/main.rs:709 —— 克隆 Arc 而非新建
let shared_config = Arc::new(parking_lot::RwLock::new(
    app.services.peri_config.read().clone()
));
// AcpServerConfig 和 ServiceRegistry 共享同一 Arc
```

**收益**：
- 消除 `sync_acp_config()`（`mod.rs:581-597`）和 `panel_manager.rs:282` 的手动写通
- 修复 `lang.rs:47-50` 和 `config_panel.rs:88-98` 两个确认的损坏点
- 保留 `session/set_config_option` 处理器（它有重建 LlmProvider/失效 agent_pool 的副作用）

**工作量**：M（需迁移 ~16 处 `services.peri_config.as_mut()` 修改点）

---

### 建议 7：Macro-based dispatch 消除 PanelState 117+ match arm 样板

**现状**：`PanelState` 13 变体在 13 个方法中各展开 13 路 match，合计 ~169 个 arm（声称 117 低估），每个 arm 仅做 `PanelState::Xxx(p) => p.<trait_method>(args)` 透传。

**建议模式**：声明式宏 dispatch

**重构草图**（`peri-tui/src/app/panel_manager.rs`）：

```rust
macro_rules! dispatch_panel {
    ($self:expr, $method:ident, $($arg:expr),*) => {
        match $self.active {
            Some(PanelState::Model(ref p)) => p.$method($($arg),*),
            Some(PanelState::Login(ref p)) => p.$method($($arg),*),
            // ... 13 个变体
            None => Default::default(),
        }
    };
}

impl PanelManager {
    pub fn dispatch_key(&mut self, input: KeyEvent, ctx: &mut PanelCtx) -> EventResult {
        dispatch_panel!(self, handle_key, input, ctx)
    }
    pub fn dispatch_mouse(&mut self, ev: MouseEvent, ctx: &mut PanelCtx) -> EventResult {
        dispatch_panel!(self, handle_mouse, ev, ctx)
    }
    // ... 其余 dispatch 方法
}
```

**收益**：新增面板只需在宏定义处加 1 行，而非 13 处。注意 `dispatch_desired_height` 的 `Some(match state)` 形态差异需宏内分支处理。

**替代方案**：更激进地改为 `Box<dyn PanelComponent>`，但会失去 enum 编译时穷举保证。

**工作量**：S（半天）

---

### 建议 8：强类型枚举保护 ThreadMeta 状态机（Making Illegal States Unrepresentable）

**现状**：`ThreadMeta.agent_status: String` 和 `cancel_policy: String`（`peri-agent/src/thread/types.rs:31,39`）是原始 String，任何字符串（包括拼写错误如 `"pendng"`）都可构造。

**已存在部分缓解**：`peri-acp/src/session/agent_runtime.rs:6-62` 已有强类型枚举 `AgentStatus`/`CancelPolicy`，但持久化层（ThreadMeta + ThreadStore + SQLite）未使用，且 `from_str` 用 `match { _ => default }` 静默强制转换无效字符串。

**建议模式**：Type State + 强类型枚举

**重构草图**：

```rust
// peri-agent/src/thread/types.rs（注意：定义在 peri-agent 而非 peri-acp，避免循环依赖）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus { Active, Done, Cancelled, Error }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CancelPolicy { Cascade, Independent }

pub struct ThreadMeta {
    pub agent_status: AgentStatus,  // 原 String
    pub cancel_policy: CancelPolicy, // 原 String
    ...
}

// 自定义 TryFrom 返回错误而非静默 fallback
impl TryFrom<&str> for AgentStatus {
    type Error = String;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "active" => Ok(AgentStatus::Active),
            // ... 其他变体
            _ => Err(format!("unknown agent_status: {}", s)),
        }
    }
}
```

**收益**：消除非法状态的可表示性，SQLite 迁移添加 `CHECK (agent_status IN ('active','done','cancelled','error'))` 约束。

**工作量**：M（需迁移 ~40 处签名）

---

### 建议 9：parking_lot::Mutex 替换 event_tx 的 std::sync::Mutex（避免 unwrap panic 风险）

**现状**：`executor.rs:271` 的 `event_tx: Arc<std::sync::Mutex<Option<UnboundedSender>>>`，后续 4 处 `.lock().unwrap()`，`close_channel` 用 `take(None)` 关闭。

**注意**：不建议改用 `(Arc<UnboundedSender>, Arc<AtomicBool>)`——会引入 TOCTOU（检查 AtomicBool 与 send() 之间 channel 可能已 drop）且破坏 `Option::take(None)` 的原子"关闭后所有 send 都 no-op"语义。

**建议模式**：改用 parking_lot::Mutex（无 poison，无 unwrap）

**重构草图**：

```rust
// executor.rs:271
let event_tx: Arc<parking_lot::Mutex<Option<UnboundedSender<_>>>> = ...;

// 所有调用点
let tx = event_tx.lock(); // 无 .unwrap()，parking_lot 直接返回 guard
if let Some(tx) = tx.as_ref() { tx.send(...).ok(); }
```

**收益**：消除 `.lock().unwrap()` 的 poisoning panic 风险，保留原子关闭语义。

**工作量**：S（2 小时，parking_lot 已是项目依赖）

---

### 建议 10：Capability Query Pattern 让 BaseModel 流式降级可观测

**现状**：`BaseModel::invoke_streaming` 默认实现回退到 `self.invoke(request).await`（`peri-agent/src/llm/mod.rs:35-41`），调用方无法知道是否真正流式。ChatAnthropic/ChatOpenAI 都 override 了，但未来新 provider 忘记 override 会静默降级。

**不建议 typestate 重构**——会破坏项目中 `Box<dyn BaseModel>` 动态分发。

**建议模式**：Capability Query

**重构草图**：

```rust
// peri-agent/src/llm/mod.rs
pub trait BaseModel: Send + Sync {
    // 新增能力查询
    fn supports_streaming(&self) -> bool { false }

    // 默认 fallback 增加可观测性
    fn invoke_streaming(...) -> impl Future<Output = ...> {
        async move {
            tracing::debug!(
                provider = self.provider_name(),
                "invoke_streaming 被调用但 provider 未实现流式，回退到非流式 invoke()"
            );
            self.invoke(request).await
        }
    }
}

// ChatAnthropic / ChatOpenAI override
impl BaseModel for ChatAnthropic {
    fn supports_streaming(&self) -> bool { true }
    ...
}
```

**收益**：降级可观测，调用方可按需 warn。

**工作量**：S

---

## 五、优先级路线图

### P0（立即，阻塞问题）

**无真正阻塞问题。** 项目功能完整，编译通过，测试覆盖核心路径。当前问题均为结构性债务，不影响运行时正确性。

但若要立即处理，**最接近阻塞的是弱点 1 的副作用**：cascade cancel 子 agent 功能在 TUI/stdio 路径下失效（`session_manager=None`）。若产品上需要 SubAgent 级别的取消传播，应优先处理。

---

### P1（近期，1-2 周，高 ROI 重构）

| 序号 | 任务 | 模式 | 工作量 | ROI 理由 |
|------|------|------|--------|----------|
| 1 | 统一会话管理到 peri-acp SessionManager | Delegation | L | 消除 5 处 frozen 重复构建，恢复 cascade cancel，goal_state 自动传播 |
| 2 | execute_prompt Parameter Object 重构 | Parameter Object + Extract Method | M | 3 个调用点简化，4 个子方法可独立测试，移除 lint 抑制 |
| 3 | rewind 命令补测试 + 修复 UTF-8 边界 bug | Strategy + Test Builder | M | 唯一零覆盖的 Immediate 命令，含文件系统破坏性操作 |
| 4 | AcpAgentConfig 渐进式子配置分组 | Facade + Builder | M | 12 字段零跨依赖可安全分组，降低高频热点变更成本 |
| 5 | Prediction 功能封装为 peri-acp Facade | Facade | S | 半天工作量，消除 [TRAP] 违规 |
| 6 | compact 命令核心路径补 Contract Test | Contract Test | M | 验证 [TRAP] 不变量（消息以 Human 开头） |

---

### P2（中期，2-4 周，质量提升）

| 序号 | 任务 | 模式 | 工作量 |
|------|------|------|--------|
| 7 | AgentComm 抽取 BgTaskState + LspDiagnostics 子结构 | Value Object Composition | M |
| 8 | 中间件内部 API 引入 thiserror 枚举（CronError/BackgroundRegistryError） | thiserror | M |
| 9 | 双 config 所有权统一为共享 Arc | Single Source of Truth | M |
| 10 | FrozenSessionData 封装为 Immutable Value Object | Immutable Value Object | S |
| 11 | PanelState dispatch 宏化 | Macro-based dispatch | S |
| 12 | ThreadMeta agent_status/cancel_policy 强类型枚举 | Type State | M |
| 13 | tool_dispatch_test.rs Mock 提取共享工厂 | Test Fixture Factory | S |

---

### P3（远期，可选，低风险清理）

| 序号 | 任务 | 模式 | 工作量 |
|------|------|------|--------|
| 14 | tool_dispatch.rs [DEADLOCK] 日志清理 + metrics 接线 | — | S |
| 15 | setup_wizard_test.rs Constructor Injection 隔离 | Constructor Injection | S |
| 16 | event_tx 改用 parking_lot::Mutex | — | S |
| 17 | BaseModel capability query（supports_streaming） | Capability Query | S |
| 18 | BaseModelReactLLM::generate_reasoning 提取 Reasoning 构建辅助 | Extract Method | S |
| 19 | ingestion_events_to_otel 提取 build_span_id 辅助 | Extract Method | S |
| 20 | CompactConfig 默认值单一来源 | — | S |
| 21 | AcpTuiClient 保留 AcpError 类型（不转 String） | — | M |
| 22 | dispatch 层 String 错误改 anyhow::Result | — | S |

---

### 不建议采纳的建议（验证后否决）

1. **用原生 async fn 替换 Middleware trait 的 async_trait**——与 `chain.rs:13` 的 `Vec<Box<dyn Middleware<S>>>` 动态分发不兼容，且实际开销（每会话几毫秒）相对 LLM 网络往返可忽略
2. **ThreadId 改为 Newtype**——ThreadId 是文本格式 ID，经 SQLite/serde/JSON-RPC 边界传输，类型别名对此场景可接受；且 `MessageId` 添加 `#[repr(transparent)]` 无实际用途（当前无 unsafe reinterpretation）
3. **AgentError::ToolExecutionFailed reason 改为 Box<dyn Error>**——会使 AgentError 失去 Clone 派生，级联影响全代码库 match 模式，代价与收益不匹配
4. **FrozenSessionData 存入 AcpSession**——会创建第三个存储点，反而增加同步负担；executor 已通过参数感知 frozen 数据

---

## 附录：关键文件索引

| 关注点 | 文件 |
|--------|------|
| 会话管理平行实现 | `peri-tui/src/acp_server/mod.rs:34-47`、`peri-acp/src/session/mod.rs:37-56` |
| God Struct | `peri-acp/src/agent/builder.rs:52-103`、`peri-tui/src/app/agent_comm.rs:26-87` |
| 超长函数 | `peri-acp/src/session/executor.rs:102-634`（execute_prompt）、`builder.rs:124-563`（build_agent） |
| Prediction 违规 | `peri-tui/src/acp_server/mod.rs:205-218` |
| rewind 零测试 | `peri-acp/src/session/command/rewind.rs` |
| Frozen data | `peri-acp/src/session/executor.rs:62-84` |
| 双 config 所有权 | `peri-tui/src/app/service_registry.rs:63`、`peri-tui/src/acp_server/mod.rs:54` |
| PanelState 样板 | `peri-tui/src/app/panel_manager.rs:136-544` |
| 错误 String 化 | `peri-middlewares/src/cron/mod.rs:50`、`tool_dispatch.rs:314-318` |
| 调试残留 | `peri-agent/src/agent/executor/tool_dispatch.rs:109,128,173,178,187,405,419` |
