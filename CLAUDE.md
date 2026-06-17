# CLAUDE.md

## 项目概述

Rust Agent 框架，8 个 Workspace Crate（含 `agm`）。

| Crate | 职责 |
|-------|------|
| `peri-agent` | 核心：ReAct 循环、Middleware trait、LLM 适配器、工具系统、持久化（SQLite）、遥测 |
| `peri-middlewares` | 中间件：文件系统、终端、HITL、SubAgent、Skills、Todo、Cron、MCP、Hooks、Plugin、LSP |
| `peri-widgets` | Widget 组件库，核心依赖 ratatui + pulldown-cmark |
| `peri-acp` | **ACP 服务层**：Agent Client Protocol 实现，通过 MpscTransport/StdioTransport 桥接 TUI/IDE 与 Agent |
| `peri-tui` | TUI 应用，依赖 peri-acp（运行时通信）+ peri-agent/middlewares/widgets（类型依赖） |
| `langfuse-client` | Langfuse 遥测客户端（独立） |
| `peri-lsp` | LSP 客户端库（独立，被 middlewares 使用） |
| `agm` | Agent Package Manager：pnpm 风格的 AI agent 依赖管理器（通过 agm.json 管理 Skills/Agents 安装） |

`rmcp` crate（v1.7）直接引用，不再需要本地 patch。

**其他目录**：`scripts/`（启动脚本）、`docs/`（博客、设计文档、协议规范，见下）、`side-projects/`（实验性项目）。

## 依赖关系

依赖关系（A → B 表示 A 依赖 B）：

- `peri-widgets`、`peri-lsp`、`langfuse-client` → 无 workspace 内部依赖（独立基础库）
- `peri-middlewares` → `peri-agent`、`peri-lsp`
- `peri-acp` → `peri-agent`、`peri-middlewares`、`peri-lsp`、`langfuse-client`
- `peri-tui` → `peri-acp`（运行时通信）+ `peri-agent`、`peri-middlewares`、`peri-lsp`、`peri-widgets`（类型依赖，用于 UI 渲染的类型如 `BaseMessage`/`ContentBlock`）

**TUI→ACP 通信**: TUI 运行时仅通过 `peri-acp` 的 `MpscTransport`（in-memory channel pair）与 ACP Server 通信。ACP Server 持有 Agent 构建和执行逻辑，TUI 作为纯 ACP client 前端消费 `AcpNotification` 事件。

## 开发命令

```bash
cargo build                          # 构建所有 crate
cargo build -p <crate>               # 构建指定 crate
cargo run -p peri-tui          # 运行 TUI
cargo run -p peri-tui -- -a    # HITL 审批模式
cargo test                           # 全量测试
cargo test -p <crate> --lib -- <test_name>  # 单个测试
lefthook install                     # 安装 git hooks
lefthook run pre-commit              # pre-commit（fmt/check/clippy）
scripts/start-tui.sh                 # 启动 TUI（RELAY_PORT=3001）
```

## 架构要点

**ReAct 循环**（`peri-agent`）：AgentInput → collect_tools → before_agent → loop(500) { before_model → LLM → after_model → [工具调用] before_tool → 并发执行 → after_tool → emit | [回答] → emit TextChunk + StateSnapshot → after_agent }。TUI 覆盖 `max_iterations(500)`（核心默认 10）。

**[TRAP]** `tool_dispatch.rs` 延迟写入：`collect_tool_results` 执行 before_tool + 并发调用 + 收集结果，**不写 state**；`dispatch_tools` 最后统一写入 AI 消息 + 所有 tool_result。禁止在 `collect_tool_results` 中调用 `state.add_message`。错误路径：before_tool 错误/Cancel 返回 `Err`（state 未修改）；执行阶段 Cancel/deferred_error 返回 `Ok((.., true, ..))`，`dispatch_tools` 写入 state 后再返回 `Err`。链上 18 个中间件的 `before_tool`/`after_tool`/`on_error` 均不读 `state.messages()`，新增中间件必须遵守。`AgentEvent::MessageAdded` 被 TUI 丢弃，TUI 通过 `StateSnapshot` + 流式事件维护状态。（详见 spec/global/domains/agent.md#issue_2026-05-15-orphaned-tool-use-after-concurrent-tool-error）

**[TRAP]** 新增/修改事件类型语义（如工具前文本从 AiReasoning 改为 TextChunk）时，必须同步检查 TUI 侧事件映射层（`map_executor_event`）。新增 AgentEvent 变体时必须同步更新映射，事件丢弃会导致下游状态不一致。（详见 spec/global/domains/agent.md#issue_2026-05-11-streaming-text-invisible-with-tools，spec/global/domains/message-pipeline.md#issue_2026-05-13-streaming-text-tool-aggregation-visual-issues）

**[TRAP]** 多工具并发的结果处理循环中，P3/P4 错误路径提前返回会导致后续 tool_result 缺失。必须用 deferred_error 模式——先收集所有错误，循环结束后统一判断。所有 tool_result 必须始终写入 state。（详见 spec/global/domains/agent.md#issue_2026-05-14-orphaned-tool-use-without-tool-result，spec/global/domains/agent.md#issue_2026-05-15-tool-execution-error-stops-agent，spec/global/domains/agent.md#issue_2026-05-18-agent-tool-calls-execute-serially）

**[TRAP]** `prepended_ids` 只追踪 `prepend_message`（头部 insert System），不能计入 `add_message`（尾部 push）。cleanup 用 `take_while(|m| m.is_system())` 只收集头部连续 System 消息，禁止用长度差计算。新增中间件的 `add_message` 注入不受 cleanup 影响，也不能假设 cleanup 会清理它们。（详见 spec/global/domains/agent.md#issue_2026-05-26-skillpreload-anthropic-400-tool-result-orphan）

**消息类型**：`BaseMessage`（Human/Ai/System/Tool），`ContentBlock`（Text/Image/Document/ToolUse/ToolResult/Unknown）。`ReasoningContentBlock` 是独立 SDK 类型，不在 `ContentBlock` 枚举中。

**LLM 适配层**：`BaseModel` trait（OpenAI/Anthropic）→ `BaseModelReactLLM` → `ReactLLM`。`RetryableLLM<L>` 指数退避重试。

**[TRAP]** `Interrupted`/`Error` + `Done` 互斥：`Interrupted`/`Error` 先 `request_rebuild()` + 添加通知，设 `reconcile_already_done=true`，后续 `Done` 跳过 `request_rebuild()` 防止覆盖通知。（详见 spec/global/domains/agent.md#issue_2026-05-25-interrupt-undo-last-user-message）**[TRAP]** Cancel 后历史不应无条件截断：ACP server 在 `result.ok==false` 时无条件 truncate history 会丢失 agent 已写入 state 的消息。应检查 `result.messages.len()` 判断是否有进展，有则保留。（详见 spec/global/domains/agent.md#issue_2026-05-26-ctrl-c-interrupt-causes-agent-amnesia，spec/global/domains/agent.md#issue_2026-05-29-llm-stream-error-causes-amnesia）**[TRAP]** executor 中 `?` 传播会跳过 `cleanup_prepended`，导致 before_agent 注入的 system 消息泄漏到 state。循环内关键 cleanup 必须用 try_break 宏将错误捕获到变量，循环后无条件执行 cleanup。（详见 spec/global/domains/agent.md#issue_2026-06-06-test-gap-llm-error-cleanup-prepended）

**系统提示词**：`build_system_prompt()` 在 `peri-acp/src/prompt/mod.rs` 中实现（通过 `concat!` 引用 `peri-tui/prompts/sections/`），`session/new` 时调用一次，产出 `frozen_system_prompt` 存入 `SessionState.frozen`，后续轮次直接复用。段落文件位于 `peri-tui/prompts/sections/`（01-06 静态 + 07, 10-15 动态，共 13 个，08/09 不存在），通过 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 边界标记分隔——标记前可缓存，标记后不影响前缀缓存。`PromptFeatures` 控制条件段落注入。Agent 构建在 system prompt 末尾追加 Git Attribution 段落（动态区域内不影响缓存前缀）。

## Thinking/推理模式

`ThinkingConfig` 控制推理参数。Anthropic 用 `thinking + output_config.effort`，OpenAI 用 `reasoning_effort`。`budget_tokens` 默认 8000，最小值 1024 的限制已移除（`--effort` 传 0 时可为 0）。`max_tokens` 必须 > `budget_tokens`。

**OpenAI Reasoning 回传**（`openai.rs`）：

- `reasoning_content` 顶层字段：所有模型无条件回传
- content 数组 `thinking` 类型：默认关闭，通过 `with_thinking_content(true)` 手动开启（如 deepseek-v4-pro）

**[TRAP]** DeepSeek `unknown variant 'thinking'`：不要把 `Reasoning` block 序列化为 `{"type":"thinking"}` 发给不支持的 provider。**[TRAP]** `reasoning_content must be passed back`：过滤 `Reasoning` 时必须同时作为顶层字段回传。两个陷阱互相关联。（详见 spec/global/domains/agent.md#issue_2026-05-12-glm-reasoning-field-not-parsed，spec/global/domains/agent.md#issue_2026-05-14-deepseek-anthropic-thinking-block-dropped，spec/global/domains/agent.md#issue_2026-05-12-thinking-reasoning-dataflow-issues）

**OpenAI 兼容适配层 Provider 特定处理**（`invoke.rs` `build_request_body`/`messages_to_json`）：

- **`reasoning` 字段已移除**：`messages_to_json` 中 assistant 消息仅回传 `reasoning_content`（OpenAI 标准字段），不再同时设置 `reasoning`。GLM 等需要 `reasoning` 字段的模型在接收端（`parse_assistant_message`）仍兼容双字段解析。
- **`stream_options` 仅 Qwen**：`stream_options.include_usage` 仅在模型名含 `qwen` 时发送，其他 provider 不发送。
- **Kimi thinking/reasoning_effort 互斥**：当 `thinking_enabled` 为 true 且模型名含 `kimi` 时，请求体中移除 `reasoning_effort`。

## 系统提示词稳定性（第一优先级）

**[原则] 系统提示词稳定性是第一优先级**：会话开始后，系统提示词必须完全稳定、不可变更。任何在会话进行中修改系统提示词的行为（包括通过 runtime config、模型切换、技能加载、中间件注入等方式间接改变其内容）都是禁止的。系统提示词内容的任何变化都会导致 Prompt Cache 失效、模型行为漂移，严重影响会话质量。

唯一例外是 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 边界标记之后动态区域内的占位符值变化（如日期、cwd），但即使是动态区域，其**结构/模板/段落数量**也必须在会话内保持不变。新增中间件在 `before_agent` 阶段注入 System 消息时，必须确保注入内容和位置跨轮次稳定。

**[TRAP]** 中途纠正/警告消息（工具连续失败提示、`<stop_hook_feedback>`、goal steering、compact 续接等）必须用 `BaseMessage::human("<system-reminder>...</system-reminder>")` 注入，**禁止** `BaseMessage::system(...)`。`anthropic/invoke.rs` 和 `openai/invoke.rs` 会遍历整个 `messages` 数组，把**所有** `BaseMessage::System` 消息（不分位置）hoist 到顶层 system prompt，导致 frozen system prompt 被污染、Prompt Cache 失效。已采用此模式的实现：`goal_middleware.rs`、`compact_middleware.rs`、`tool_dispatch.rs`（连续失败警告）、`hooks/middleware.rs`（stop_hook_feedback）。新增中途纠正路径必须复用同一模式。（详见 spec/global/domains/system-prompt.md#issue_2026-06-17-mid-conversation-system-message-breaks-frozen-prompt）

**[TRAP]** SubAgent 中间件链必须复用 main agent 在 `session/new` 时捕获的 frozen CLAUDE.md/Skills 数据，禁止重新读盘。否则会话中 CLAUDE.md/skills 文件变更会让 SubAgent 看到与 main agent 不同的内容，违反第一优先级不变量。frozen 数据通过 `SubAgentMiddleware::with_frozen_data` → `SubAgentTool::with_frozen_data` → `SubAgentMiddlewareConfig::with_frozen` → `build_subagent_middlewares` 透传，最终调用 `AgentsMdMiddleware::with_frozen_content` / `SkillsMiddleware::with_frozen_summary`。`Option<Arc<String>>` 共享避免每轮 `build_tool` 重复 clone 大字符串。（详见 spec/global/domains/system-prompt.md#issue_2026-06-17-subagent-ignores-frozen-claude-md）

## Tool Search 延迟加载

工具分三层：**Core（12 个）**——Read/Write/Edit/Glob/Grep/folder_operations/Bash/WebFetch/WebSearch/Agent/AskUserQuestion/TodoWrite，始终对 LLM 可见；**Meta（2 个）**——`SearchExtraTools`/`ExecuteExtraTool`，始终可见，用于按需发现和执行 deferred tools；**Deferred（其余）**——Cron*、MCP 工具、LspTool 等，LLM 不直接可见，通过 Meta 工具桥接。核心工具定义以 `tool_search/core_tools.rs` 中的 `CORE_TOOLS` 为准。新增工具优先配置为 deferred tool，避免膨胀核心工具列表。

**[TRAP]** `Box<dyn BaseTool>` 不能直接转 `Arc<dyn BaseTool>`，用 `box_to_arc()` 通过 `ToolWrapper(ManuallyDrop<Box>)` 透传。**绝不能用 `Box::into_raw` + `Arc::from_raw`**——布局不同导致 UB。

**[TRAP]** Prompt Cache 前缀稳定性——通用原则：所有参与缓存前缀的数据（system prompt、tools 数组、消息顺序）必须保证跨请求稳定。具体规则：

- （a）system prompt 中用 `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 边界标记分隔静态/动态内容，标记前可缓存，标记后不缓存
- （b）非 System 消息必须用 `add_message`（尾部追加），禁止 `prepend_message`（头部插入会改变 Anthropic cache_control 标记位置）；System 消息用 `prepend_message` 插入头部是安全的
- （c）动态占位符（日期、cwd、环境变量）放在边界标记之后
- （d）middleware 注入的 System 消息天然在边界标记之后（非缓存块）

历史踩坑已全部修复并固化到 frozen_system_prompt + cached_prompt + boundary 标记机制中。（详见 spec/global/domains/message-pipeline.md，spec/global/domains/system-prompt.md#issue_2026-05-23-mcp-tools-instability-breaks-anthropic-cache）

**[TRAP]** `prepend_message` 的 `insert(0)` 右移导致 StateSnapshot 快照范围扩大，泄露 System 消息到 `agent_state_messages`。StateSnapshot 应始终 `.filter(|m| !m.is_system())`。（详见 spec/global/domains/system-prompt.md#issue_2026-05-13-system-prompt-dynamic-parts-duplicated-in-consecutive-calls）

### 新增/删除 Core 工具检查清单

新增或删除 Core 工具时，必须同步更新以下触点（漏改任一项都会导致 LLM 工具列表与文档/MCP/HITL 不一致）：

1. `peri-middlewares/src/tool_search/core_tools.rs` —— `CORE_TOOLS` HashSet + 对应 `TOOL_*` 常量
2. `peri-middlewares/src/tool_search/search_tool.rs` & `execute_tool.rs` —— 已改为通过 `core_tools_sorted_csv()` 动态生成（P1-1），无需手改，但需确认 description 内容正确
3. `peri-tui/prompts/sections/05_using_tools.md` —— 用户可见的"选择正确工具"指引，必须列出该工具
4. `peri-middlewares/src/hitl/mod.rs` —— `is_edit_tool()` 与默认审批列表（如工具涉及文件修改/命令执行）
5. `peri-acp/src/event/mapper.rs`（或对应路径）—— `ToolKind` 映射，决定 TUI 图标显示
6. `peri-tui/src/tool_display.rs` —— TUI 简称/全称映射
7. `core_tools_test.rs` —— 更新"动态生成 CSV 包含所有工具"的断言列表
8. 若是 Edit/Write 类工具，额外检查 `GitAttributionMiddleware` 的 before_tool/after_tool 钩子是否需要追踪

删除 Core 工具时反向操作，且必须 grep 全仓库确认无残留引用。

## 中间件链执行顺序

详见 `peri-middlewares/CLAUDE.md`。18 个中间件按固定顺序组成链，末尾 `[ReActAgent.with_system_prompt()]` prepend。

## ACP/TUI 分层架构

**概述**：`peri-acp` 是独立的 ACP 服务层 crate。`peri-tui` 为纯 ACP client 前端，通过 `MpscTransport` 与 `peri-acp` 通信。详见 `peri-tui/CLAUDE.md`。

**数据流**（详见 `peri-tui/CLAUDE.md`）：
- TUI 路径：TUI 输入 → AcpTuiClient → MpscTransport → ACP Server → executor → ExecutorEvent → TransportEventSink → TUI UI 更新
- Stdio 路径：SDK → executor + StdioEventSink → stdout JSON-RPC

Stdio 和 TUI 路径共享 `executor::execute_prompt()`。Stdio 当前支持：`session/new`、`session/prompt`、`session/cancel`、`session/set_config_option`。新增 ACP 方法时必须检查两条路径是否都需要实现。

**[TRAP]** Stdio `initialize` 响应必须声明 session capabilities（与 TUI 路径的 `AcpServerConfig` 对齐）。

**Frozen Data Flow**（会话内不可变数据）：

**真正冻结（session/new 一次性捕获，存 SessionState.frozen_*）：**
```
session/new → frozen_date → frozen_claude_md + frozen_claude_local_md
            → frozen_skill_summary → frozen_system_prompt → SessionState.frozen_*
            → executor::execute_prompt → AcpAgentConfig.frozen_*
```

**每轮 prompt 重新计算（非冻结）：**
- `is_git_repo`：实时检查 `.git` 目录
- `YOLO_MODE`：每次 SubAgent 构建时重新读取
- `DISABLE_COMPACT` / `DISABLE_AUTO_COMPACT` / `COMPACT_THRESHOLD`：每轮读取 env
- `peri_config`、Provider Snapshot、context_window：每轮从 `Arc<RwLock<>>` 克隆快照
- 整个中间件链、AgentState、Cancel Token、Langfuse Tracer：每轮全新构造

**[TRAP]** `PromptFeatures::detect()` 与 SubAgent 漂移：`PromptFeatures::detect()` 仍每轮重新读取 `YOLO_MODE`，`is_git_repo` 也每轮重新检查——两者未随 frozen 数据传递。这违反"系统提示词稳定性第一优先级"的派生不变量：SubAgent 在会话进行中可能因 env 变化或 git 状态变化而看到与 Main Agent 不同的 `PromptFeatures`，从而注入不同的 prompt 段落。新增依赖 `PromptFeatures` 的中间件时必须明确：（1）该特征应否被 frozen；（2）若不 frozen，SubAgent 链路是否会因此与 Main Agent 漂移；（3）漂移是否会破坏 prompt cache 前缀。详见 spec/global/domains/system-prompt.md#issue_2026-05-27-language-injection-subagent-drift-cache-isolation。

**ACP Slash Commands**（符合 agentclientprotocol.com）：
- `CommandKind`（`Immediate`/`Passthrough`/`Transform`）分类执行
- `/compact`：手动触发 full compact（`CommandKind::Immediate`，文件 `peri-acp/src/session/command/compact.rs`）
- `/rewind <message_id>`：回滚对话到指定消息，逆向恢复 Write/Edit 文件变更（`CommandKind::Immediate`，文件 `peri-acp/src/session/command/rewind.rs`）
- `/bg <任务描述>`：后台启动 Fork Agent 执行独立任务（`CommandKind::Immediate`，文件 `peri-acp/src/session/command/bg.rs`）
- `/clear` 保留为 UICommand（`app.new_thread()` 创建新 session），不走 ACP
- **[TRAP]** Immediate 命令路径绕过 agent event pump，必须手动调用 `sink.push_done()`。（详见 spec/global/domains/agent.md#issue_2026-05-29-immediate-command-missing-push-done）

**[TRAP]** Agent 构建和执行统一通过 `peri_acp::session::executor::execute_prompt()`。禁止在 TUI 层直接构建 ReActAgent 或手写事件泵。`build_agent()` 每轮重建的大对象已通过 `AgentPool` session 级缓存复用。（详见 spec/global/domains/agent.md#issue_2026-05-24-build-agent-per-turn-arc-transient-fragmentation）

**[TRAP]** TUI 层数据必须通过 ACP 协议到达 ACP 层，禁止直连。（详见 spec/global/domains/agent.md#issue_2026-05-29-clear-keeps-acp-server-history）

**[TRAP]** Session Config Options 覆盖旧的 Session Modes API。`build_config_options()` 必须按优先级顺序返回（mode → model → thinking_effort）。

## 上下文压缩

**架构**：Compact 由 `CompactMiddleware` 在 ReAct 循环内通过 `before_model` 钩子就地处理。

**触发**：`CompactMiddleware::before_model` 检查 `ContextBudget`：0.70 micro-compact，0.85 full compact。环境变量覆盖：`DISABLE_COMPACT`、`DISABLE_AUTO_COMPACT`、`COMPACT_THRESHOLD`（0.0-1.0）。

**核心文件**：
| 文件 | 职责 |
|------|------|
| `peri-agent/src/agent/compact/` | `full_compact()`/`micro_compact_enhanced()`/`re_inject()`/`config`/`invariant` |
| `peri-middlewares/src/compact_middleware.rs` | `CompactMiddleware`：`before_model` 钩子 |
| `peri-acp/src/session/command/compact.rs` | `/compact` Slash Command（`CommandKind::Immediate`） |
| `peri-acp/src/session/command/rewind.rs` | `/rewind` Slash Command（`CommandKind::Immediate`） |
| `peri-acp/src/session/command/bg.rs` | `/bg` Slash Command（`CommandKind::Immediate`） |

**[TRAP]** compact 后消息结构必须以 `BaseMessage::human(summary + continuation)` 开头。禁止将摘要放在 `BaseMessage::system()` 中。compact 后的完整结构：`[Human(摘要+续接指令), System(文件)..., System(Skills)...]`。（详见 spec/global/domains/compact.md#issue_2026-05-20-auto-compact-empty-messages-400）

**[TRAP]** full compact 后 `preprocess_messages` 处理 Ai 消息时只保留工具调用名称、完全丢弃参数（含 file_path 等路径信息），摘要 LLM 无法知道操作的是哪个文件。`full_compact()` 增加 `cwd` 参数为摘要 LLM 提供路径锚点。（详见 spec/global/domains/compact.md#issue_2026-06-07-full-compact-loses-project-path-context）

## Sync 模块

**[TRAP]** 路径穿越防护：`validate_and_resolve()` 是项目标准的路径穿越防护入口。任何需要接收用户侧相对路径并写入 base_dir 的场景都必须复用此函数。新增类似写入功能时禁止自行实现路径校验。

**[TRAP]** `Path::strip_prefix()` + `to_string_lossy()` 在 Windows 上产生 `\` 分隔符，sync 协议要求 `/`。`scan_dir_recursive()` 构造 `FileEntry.path` 时必须 `.replace('\\', "/")`。（详见 spec/global/domains/sync.md#issue_2026-05-20-windows-path-separator-breaks-tests）

`peri sync` 子命令使用标准终端 CLI（crossterm 交互），不经过 TUI 主循环。

## 文档目录

| 目录 | 内容 |
|------|------|
| `docs/blogs/` | 技术博客（streaming-render、compact-mechanism、prompt-cache 等 31+ 篇） |
| `.claude/skills/blog-writer/SKILL.md` | 博客写作风格指南，写博客时触发 blog-writer skill |
| `docs/superpowers/specs/` | Superpowers 插件设计规范 |
| `docs/superpowers/plans/` | Superpowers 插件实现计划 |
| `docs/acp/` | ACP 协议文档（实现报告、协议差距分析） |

## 环境变量

| 变量 | 说明 |
|------|------|
| `ANTHROPIC_API_KEY` | Anthropic API Key |
| `ANTHROPIC_BASE_URL` | Anthropic 自定义 Base URL |
| `ANTHROPIC_MODEL` | 默认 Anthropic 模型名 |
| `OPENAI_API_KEY` | OpenAI 兼容 API Key |
| `OPENAI_API_BASE` | API Base URL（优先于 `OPENAI_BASE_URL`） |
| `OPENAI_BASE_URL` | API Base URL（fallback） |
| `OPENAI_MODEL` | 模型名称（默认 gpt-4o） |
| `MODEL_PROVIDER` | Provider 选择提示（auto-detect） |
| `YOLO_MODE=true/false` | 跳过/启用 HITL 审批 |
| `RUST_LOG` | 日志级别（默认 info） |
| `RUST_LOG_FORMAT` | `"json"` 时输出 JSON 格式日志 |
| `RUST_LOG_FILE` | 日志文件路径 |
| `LANGFUSE_PUBLIC_KEY` | Langfuse 公钥（缺一则禁用遥测） |
| `LANGFUSE_SECRET_KEY` | Langfuse 密钥 |
| `LANGFUSE_BASE_URL` | Langfuse 服务地址（默认 cloud.langfuse.com） |
| `DISABLE_COMPACT` | 禁用所有 compact（含 auto + micro） |
| `DISABLE_AUTO_COMPACT` | 仅禁用 auto compact |
| `COMPACT_THRESHOLD` | 覆盖 auto compact 阈值（0.0-1.0，默认 0.85） |

配置通过 `~/.peri/settings.json` 的 `env` 字段注入。

## Beta 功能开关

`settings.json` → `config.betas` 控制 beta 功能。所有字段默认 `false`。当前无活跃 beta 功能。

## CLI 参数

对齐 Claude Code 核心参数体系。所有 camelCase 参数同时支持 kebab-case 别名。clap 4 derive 解析。

**参数列表**：

| 参数 | 说明 | 模式 |
|------|------|------|
| `-p/--print [PROMPT]` | 非交互模式：执行单轮问答后输出到 stdout 并退出 | print only |
| `--output-format` | 输出格式：text / json / stream-json（配合 `-p`） | print only |
| `--max-turns` | 最大 agentic 轮数（配合 `-p`） | print only |
| `--bare` | 极简模式：跳过 hooks/LSP/插件/MCP 初始化（配合 `-p`） | print only |
| `--permission-mode` | 权限模式：bypass / default / dont-ask / accept-edit / auto-mode | both |
| `--dangerously-skip-permissions` | 绕过所有权限检查（等同 permission-mode bypass） | both |
| `--model` | 指定模型（别名如 sonnet 或全名） | both |
| `--effort` | 推理强度：low / medium / high / max | both |
| `-c/--continue` | 继续当前目录最近的对话 | TUI |
| `-r/--resume [ID]` | 按 session ID 恢复对话 | TUI |
| `--session-id` | 指定会话 ID | TUI |
| `-n/--name` | 设置会话显示名称 | TUI |
| `--no-session-persistence` | 禁用会话持久化 | TUI |
| `--allowedTools` | 允许的工具列表 | both |
| `--disallowedTools` | 禁止的工具列表 | both |
| `--settings` | 加载额外 settings 文件或 JSON 字符串 | both |
| `-a/--approve` | 启用 HITL 审批模式（等同 --permission-mode default） | TUI |
| `-y/--yolo` | 向后兼容，无操作（YOLO 已是默认行为） | TUI |

**子命令**：`plugin list [--json]` / `plugin install <name@marketplace> [--scope user/project/local]` / `plugin uninstall <id>`。`acp`/`update`/`sync` 子命令保持不变。

**`-p` 模式架构**：复用 ACP executor，通过 `PrintEventSink` 收集事件并输出。不启动 TUI、不维持 session。`PrintBroker` 自动批准所有交互。

运行时 `Shift+Tab` 切换权限模式，`Ctrl+T` 切换模型，`Ctrl+Shift+T` 切换 Provider。

## 编码规范

- Rust 2021 edition，tokio async/await + async-trait
- 库用 `thiserror`，应用层用 `anyhow::Result`
- 日志用 `tracing`，禁止 `println!`/`eprintln!`
- 测试与源码分离为同目录 `_test.rs` 文件（≥30 行必须分离）
- bin crate 集成测试在 `src/` 内（不支持 `tests/` 目录）
- 每模块一目录，`mod.rs` 入口；Workspace resolver = "2"，禁止下层依赖上层
- 禁止 `ℹ`（U+2139）符号和 `[i]` 前缀
- **字符串截断必须用字符级操作**：`s.chars().take(N).collect()` 或 `s.char_indices().nth(N)`，`&s[..N]` 对 CJK 会 panic
- 终端列宽用 `unicode-width` crate（CJK 占 2 列）
- **终端 UI 鼠标坐标转换**：鼠标事件坐标是显示列（unicode-width），光标位置是字符索引，需逐字符累加转换。（详见 spec/global/domains/tui.md#issue_2026-05-12-textarea-mouse-click-cursor-misposition-cjk）
- **快捷键设计**：禁止 `Shift+字母`（编辑态等同大写输入）。全局用 `Ctrl+字母`，面板用方向键/Space/Enter/Esc。
- **快捷键跨平台兼容 [TRAP]**：`Alt+Enter`/`Alt+M` 在 Windows 终端被截获，新增快捷键必须优先用 `Ctrl+字母`，避免 `Alt` 修饰键。
- **面板系统**：`PanelManager` + `PanelComponent` trait，新增面板只需定义变体 + 实现 trait。面板内禁止渲染提示行，由 `status_bar_hints()` 统一描述。
- **`Event::Paste`**：独立于 key event 链，必须单独拦截。
- **翻页快捷键**：不使用 PageUp/PageDown。滚动统一用 `Ctrl+U`/`Ctrl+D`（textarea 空时）。禁止添加 PageUp/PageDown 滚动行为。
- **鼠标事件合并**：`coalesce_mouse_events()` 对连续 Scroll/Drag 事件做非阻塞 drain 合并，只保留最后一个。

## 测试编写风格

- 注释、断言消息用中文；命名 `test_<被测对象>_<场景>`
- Arrange-Act-Assert，无空行分隔
- 断言优先 `assert_eq!`/`assert!`，`.unwrap()` 仅用于构造测试数据
- Mock 命名 `make_` 前缀（函数），`Mock` 前缀（结构体），不跨文件共享
- 最小依赖：`assert!`/`assert_eq!`/`matches!` + `tempfile` + `tokio-test`

## 开发注意事项

- **测试隔离**：禁止写入全局配置。用 `App::save_config(cfg, self.config_path_override.as_deref())`。
- **`std::sync::RwLockReadGuard` 不是 `Send`**，async 中不能跨 `.await` 持有，用 `parking_lot::RwLock`。
- **`CommandRegistry::dispatch` 借用限制 [TRAP]**：`&self` + `&mut App` 冲突，当前用 `std::mem::take` + put-back 解决。
- **`ServiceRegistry` 与 `GlobalUiState`**：`App` 状态拆分为 `ServiceRegistry`（跨会话共享）和 `GlobalUiState`（纯 UI 临时状态）。面板 dispatch 宏位于 `event/macros.rs`。
- **`app/mod.rs` 模块组织**：使用标准 `mod`/`pub mod` 声明按功能类别分组。
- **跨平台 spawn [TRAP]**：所有子进程 spawn 必须通过 `shell_command()` 统一 wrapper，Windows 用 powershell `-NoProfile -NonInteractive -NoLogo -Command`、Unix 用 `bash -c`。新增 spawn 时必须复用。
- **MultiplexBroker 竞速 [TRAP]**：ChannelBroker 不支持 Questions 交互类型，不应与 TUI broker 参与竞速。（详见 spec/global/domains/agent.md#issue_2026-05-29-ask-user-tool-auto-complete）