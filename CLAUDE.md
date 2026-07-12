# CLAUDE.md — Perihelion（perī）

终端 AI 编程助手。v2 单路径，`build_and_execute_agent_v2` → `run_react_loop`。

## 架构速览

### Crate 拓扑
```
peri-tui（TUI 前端） → peri-acp（服务层） → peri-agent（ReAct 引擎）
                                               peri-middlewares（19 个中间件）
peri-widgets（组件库）  langfuse-client  peri-lsp  peri-web-pty  agm
```

### ReAct 循环
```
before_agent → loop(500):
  Compact（ContextBudget: 0.70 micro / 0.85 full）
  → Receive → Reason → Act（3 阶段工具分发） → End（检查 MessageQueue 续跑）
```

Compact 由 `stages/compact.rs` 统一处理；MessageQueue 作为 cron/channel/workflow/bg 统一异步消息通道。

### TUI 数据流
```
ACP 事件 → acp_notifier → acp_bridge → VIEW_MODELS atom
                                            ↓
                           message_area 直接消费
                             ├─ vm_to_lines → Vec<Line>
                             ├─ wrap_map_cache → 视口裁剪
                             └─ ScrollThrottle 16ms 节流
```
VIEW_MODELS = `{ items: im::Vector<TuiRenderUnit>, generation: u64 }` 是唯一数据源。

### 上下文缓存
SP 结构不可变（破坏 prompt cache）。`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 分隔静态/动态。
- Frozen Data Flow：`frozen_date → frozen_claude_md → frozen_skill_summary → frozen_system_prompt`
- 中途纠正消息用 `BaseMessage::human()`，禁止 `BaseMessage::system()`
- SubAgent 复用 main agent frozen 数据，禁止重新读盘
- 非 System 消息用 `add_message`，禁止 `prepend_message`

### 中间件链
14 基础 + 5 条件（Hook/MCP/Workflow/LSP/Goal），链末尾 `with_system_prompt()` prepend。顺序不可重排。

### Tool Search
三层：Core（12，始终可见）/ Meta（2，SearchExtraTools/ExecuteExtraTool）/ Deferred（Cron/MCP/LspTool 等）

## 模块索引

### peri-agent（ReAct 引擎）
| 文件 | 职责 | 消费方 |
|------|------|--------|
| `agent/stages/` | ReAct 循环 5 阶段（reason/act/compact/receive/end） | run_react_loop |
| `agent/compact_v2.rs` | compact 入口（micro + full） | stages/compact + /compact 命令 |
| `llm/{openai,anthropic}/invoke.rs` | Provider 特定处理 + System hoist | BaseModelReactLLM |
| `messages/` | BaseMessage + ContentBlock（含 Reasoning） | 全链 |
| `middleware/chain.rs` | 链构造 + collect_tools | builder |
| `error_suggest/` | 工具错误建议注入 | tool_dispatch |

### peri-acp（服务层）
| 文件 | 职责 | 消费方 |
|------|------|--------|
| `agent/builder.rs:490` | 中间件链构造（14+5 固定顺序） | execute_prompt |
| `agent/builder_v2.rs` | StageContext 构建 | builder |
| `session/executor.rs` | `execute_prompt()` 统一入口 | TUI/Stdio |
| `prompt/mod.rs` | `build_system_prompt()` + 动态边界 | session/new |
| `event/{router,mapper,view_mapper}.rs` | ExecutorEvent → SessionUpdate 路由 | acp_notifier |
| `prompts/sections/` | 14 个系统提示词段落（.md） | prompt/mod |

### peri-tui（TUI 前端）
| 文件 | 职责 | 消费方 |
|------|------|--------|
| `kit/entry.rs` | 入口 + 4 主链路 spawn | main |
| `kit/atoms.rs` | VIEW_MODELS/SERVICE_SNAPSHOT 等全局 Atom | 全 TUI |
| `kit/acp_bridge.rs` | BridgeState → Atom 写入 | acp_notifier |
| `kit/acp_events.rs` | `push_view_models`: BridgeState → VIEW_MODELS | acp_bridge |
| `kit/acp_notifier.rs` | ACP 通知 → AcpEventData → bridge_tx | entry |
| `kit/message_area/` | 消息区渲染（视口裁剪+节流）；子模块 mod/render/selection/scroll/footer/props | 读 VIEW_MODELS |
| `kit/input_area.rs` | 输入框（多行/history/@mention/slash） | 写 SUBMIT_TX |
| `kit/panels/` | 15 面板（Model/Config/Cron/ThreadBrowser...） | app/panel_types |
| `kit/popups/` | 4 弹窗（HITL/AskUser/Rewind/OAuth） | acp_events |

### peri-middlewares（中间件集合）
详见 `peri-middlewares/CLAUDE.md`（完整链执行顺序、MCP 配置、插件系统、SubAgent、LSP）。

## 开发

### 命令
- `cargo build --workspace` / `cargo build -p <crate>`：构建
- `cargo test --workspace` / `cargo test -p <crate> --lib -- <test_name>`：测试
- `cargo run -p peri-tui -- -a`：HITL 审批模式
- `scripts/start-tui.sh`：启动（RELAY_PORT=3001）
- `lefthook run pre-commit`：fmt/check/clippy

### 编码规范
- Rust 2021 + async-trait。库 `thiserror`/`tracing`，应用 `anyhow`。禁止 `println!`
- 测试分离 `_test.rs`（≥30 行）/ `#[cfg(test)] mod tests`（<30 行）。每模块一目录 `mod.rs`
- 字符串截断用 `chars().take(N)`（CJK 安全）。终端列宽用 `unicode-width`
- 快捷键：禁止 `Shift+字母`/PageUp/Down；优先 `Ctrl+字母`；禁止 `ℹ`（U+2139）

### 测试规范
详见 `docs/design/testing-standards.md`。P0：serde/事件映射/纯逻辑/工具错误路径/中间件链；P1：状态机/协议/异步/安全/Prompt；不测 TUI render body/外部 API。Mock 用 `make_` 前缀手写 trait impl。

### 环境变量
`~/.peri/settings.json` env 字段注入。Provider：`ANTHROPIC_*`/`OPENAI_*`。行为：`YOLO_MODE`/`DISABLE_COMPACT`。遥测：`LANGFUSE_*`。

### Theme
颜色从 `peri-theme` 三 Atom（`THEME_ATOM`/`PALETTE_ATOM`/`PERI_COLORS_ATOM`）获取，禁止硬编码色值。`#[component]` 用 `hooks.use_atom`，非 component 两步绑定防悬垂引用。

### Workflow 排障
"0 agents, 0 tool calls"：`which peri-workflow` 可用 → `npm install && npm run build` → `cargo build -p peri-workflow` → 重启。

## 陷阱速查

### Agent 循环
- **drain_for_end**：executor 末尾禁止，消息物理丢失。idle 续跑由 TUI 负责
- **await_wake**：`run_session_loop` 末尾禁止，stdio 收不到响应
- **before_tool/after_tool**：不读 `state.messages()`，`tool_dispatch.rs` 延迟写入
- **AgentEvent 变体**：新增需同步 `map_executor_event`（peri-acp/event + peri-tui/acp_events）
- **Interrupted/Error vs Done 互斥**：前者 `request_rebuild()` + `reconcile_already_done=true`
- **ACP 通知覆盖度**：所有事件（含 AgentDone→TurnDone）必须完整转发，遗漏→UI 卡死
- **PromptFeatures**：`detect()` 每轮读 `YOLO_MODE`/`is_git_repo`，未 frozen
- **Immediate 命令**：绕过 event pump，必须手动 `sink.push_done()`
- **中途纠正消息**：用 `BaseMessage::human()`，禁止 `BaseMessage::system()`（invoke.rs 会 hoist 污染 frozen prompt）

### ACP/TUI 分层
- **execute_prompt**：Agent 构建统一入口，禁止 TUI 直连运行时
- **SubAgent frozen**：必须复用 main agent frozen 数据，禁止重新读盘
- **add_message vs prepend_message**：非 System 消息用 `add_message`（尾部追加）
- **`_meta` key**：ACP SDK 序列化 key 是 `"_meta"` 非 `"meta"`。`is_session_replay` 检测用四级 fallback（`_meta`→`meta`→`content._meta`→`content.meta`）
- **session replay DTO**：复用正常流式路径数据结构，不要发明 replay 专用变体

### TUI 渲染
- **use_* 顺序**：`hooks.use_*` 必须在 `if`/`match`/`return` 之前，顺序/数量变化→`"Hook type mismatch"` panic
- **render body 写 atom**：禁止（含 `use_effect`），`ReactiveMutRef::Drop` 无条件 `wake()` → 自激回路
- **use_state write_no_update**：render body 内 `use_state` 写入必须用 `write_no_update()`，不是 `write()`
- **BRIDGE_RESET_COUNTER**：/clear 和 thread 切换前必须递增，仅 atom 重置不足
- **overlay 空态**：返回 `Positioned(width:0, height:0)`，不要 `View()`/`Fragment` → 白屏
- **事件边界**：消息区只处理鼠标滚轮，编辑区只处理键盘

### SubAgent / Worktree
- **coder cwd**：不遵守 `Agent(cwd=...)`，prompt 中必须用绝对路径。push 前 `git diff --stat` 确认
- **文件白名单**：coding prompt 必须列文件白名单 + `DO NOT modify:` 禁止清单

### Rust / 编码
- **rustfmt import**：`use crate::module::*` 通配导入排在单类型之前；跨 crate 同理
- **CJK 截断**：用 `chars().take(N)`，`&s[..N]` 对中文 panic
- **u16 坐标**：用 `saturating_add`/`saturating_sub`，禁止裸 `+`/`-`
- **RwLockReadGuard Send**：不是 Send，async 跨 `.await` 用 `parking_lot::RwLock`
- **剪贴板**：阻塞系统 I/O 用 `std::thread::spawn` 独立线程
- **Paste 事件**：`Event::Paste` 独立于 key event，需 BracketedPaste
- **Theme 悬垂引用**：`&THEME_ATOM.state().read().xxx` 崩，须两步绑定；`theme_def.semantic` 不能直接访问，须 `theme_def.read().semantic`

## 任务入口矩阵

| 任务 | 入口文件 | 注意事项 |
|------|---------|----------|
| 新增/删除 Core 工具 | `tool_search/core_tools.rs:38` 的 `CORE_TOOLS` 常量 | 同步 6 处：prompt §05、HITL 审批列表、event/mapper、tool_display、core_tools_test、GitAttribution |
| 新增中间件 | `peri-acp/src/agent/builder.rs:490` | 14+5 固定顺序，禁止重排 |
| 改 LLM Provider 调用 | `peri-agent/src/llm/{openai,anthropic}/invoke.rs` | System hoist 规则：禁止 `BaseMessage::system()` 中途注入 |
| 新增 TUI 面板 | `peri-tui/src/kit/panels/` → `app/panel_types.rs:7` 的 `PanelKind` | 用 `panel_shell!` 宏 + `MutexGroup` 分组 |
| 改 系统提示词 | `peri-acp/prompts/sections/`（14 个 .md 段落） | 静态段结构不可变（破坏 prompt cache） |
| 改 TUI 渲染 | `message_area/`（主渲染，子模块 mod/render/selection/scroll/footer/props）+ `acp_bridge.rs`（事件→状态）+ `acp_events.rs`（push_view_models） | VIEW_MODELS 是唯一数据源 |
| 改 SubAgent | `peri-middlewares/src/subagent/`（工具/构建器/spawner/v2_bridge） | frozen 数据必须从 main agent 透传 |
| 改 MCP 配置 | `peri-middlewares/src/mcp/`（initialize/reconnect）+ `~/.peri/settings.json` | 三层合并：全局→插件→项目 `.mcp.json` |
| 改 Plugin 系统 | `peri-middlewares/src/plugin/`（installer/marketplace/config） | 兼容 Claude Code 生态 |
| 改 Skills | `peri-middlewares/src/skills/`（扫描/叶子语义）+ `skills/builtin/`（编译期嵌入） | 搜索顺序：用户→项目→插件→Builtin |
