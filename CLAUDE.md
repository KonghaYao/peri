# CLAUDE.md — Perihelion（perī）

终端 AI 编程助手。v2 单路径，`build_and_execute_agent_v2` → `run_react_loop`。

## 架构速览

### Crate 拓扑
```
peri-tui（TUI 前端） → peri-acp（服务层） → peri-agent（ReAct 引擎）
                                               peri-middlewares（20 个中间件）
peri-widgets（组件库）  langfuse-client  peri-lsp  peri-web-pty  agm
peri-acp-types（协议类型）  peri-workflow（Workflow CLI，独立构建）
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
15 基础 + 5 条件（Hook/MCP/Workflow/LSP/Goal），链末尾 `with_system_prompt()` prepend。顺序不可重排。

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
| `tools/mod.rs` | BaseTool trait（含 `aliases()` 工具别名声明） | 全链 |

### peri-acp（服务层）
| 文件 | 职责 | 消费方 |
|------|------|--------|
| `agent/builder.rs:490` | 中间件链构造（15+5 固定顺序） | execute_prompt |
| `agent/builder_v2.rs` | StageContext 构建 | builder |
| `session/executor.rs` | `execute_prompt()` 统一入口 | TUI/Stdio |
| `prompt/mod.rs` | `build_system_prompt()` + 动态边界 | session/new |
| `event/{router,mapper,view_mapper}.rs` | ExecutorEvent → SessionUpdate 路由 | acp_notifier |
| `langfuse/tracer/` | per-turn 追踪器：tool_batch（per-act flush）→ ObservationType::Tool | executor_helpers |
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
| `kit/panels/` | 16 面板（Model/Config/Cron/ThreadBrowser/Theme...） | app/panel_types |
| `kit/popups/` | 6 弹窗（HITL/AskUser/Rewind/OAuth/Confirm/Download） | acp_events |

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

#### 通用
- **Rust 2021** edition。crate 内根 `#![allow(clippy::xxx)]` 需注释原因
- **禁止 `println!`/`eprintln!`/`dbg!`**，统一用 `tracing`（`tracing::info!(target: "xxx", ...)`）
- **注释/断言用中文**；doc comment 用英文或中文均可，但同一模块内保持一致

#### 错误处理
- **库 crates**（peri-agent / peri-middlewares / peri-lsp ...）：用 `thiserror` 定义结构化 error enum + `type XxxResult<T> = Result<T, XxxError>`
- **应用 crates**（peri-tui / peri-acp）：用 `anyhow::Result`
- `#[error("...")]` 消息用英文（方便 grep），含上下文字段用 `{field}` 插值
- `#[from]` 自动转换仅用于真正等价的错误类型，其余手动 map

#### Async
- trait 方法用 `#[async_trait]`；运行时 `tokio`（`features = ["full"]`）
- 跨 `.await` 持锁用 `parking_lot::RwLock`（std `RwLockReadGuard` 不是 `Send`）
- 阻塞系统 I/O（剪贴板等）用 `std::thread::spawn` 独立线程，禁止在 async 中直接阻塞

#### 字符串与数值
- **CJK 截断**：`s.chars().take(N).collect::<String>()`，禁止 `&s[..N]`（中文 panic）
- **终端列宽**：用 `unicode_width::UnicodeWidthStr` / `UnicodeWidthChar`，禁止 `.len()` 当宽度
- **u16 坐标**：`saturating_add` / `saturating_sub`，禁止裸 `+` / `-`

#### 导入与模块
- 每模块一个目录，`mod.rs` 中 `pub use` 做预导出缩短调用路径
- `use crate::module::*` 通配导入排在单类型之前；跨 crate 同理（rustfmt 约定）
- 不设 `rustfmt.toml`，依赖 cargo 默认格式化

#### 快捷键
- 禁止 `Shift+字母` / PageUp / Down；优先 `Ctrl+字母`；禁止 `ℹ`（U+2139）

#### i18n（TUI 界面翻译）

**范围界定**：**只翻译 TUI 界面中用户会读到的文本**，其余一律保持英文原样。

| 需要 i18n | 不需要 i18n |
|-----------|-------------|
| 面板标题/描述/导航提示 | `tracing` 日志（`info!`/`warn!`/`error!` 等） |
| 弹窗标题/内容/操作提示 | `thiserror`/`anyhow` error message 字符串 |
| 通知消息（写入 NOTIFICATION atom） | ACP 协议层错误字符串 |
| 消息流内嵌标签（提醒 tag、渠道名、工具别名） | Markdown box-drawing 字符和格式符号 |
| 向导/欢迎页/配置页文本 | 代码内变量名/字段名/路径字符串 |
| 空态/占位符/统计文本 | `println!`/`eprintln!` CLI 终端输出 |

**基础设施**：`peri-tui/src/i18n/mod.rs`（Fluent），API：
- `i18n::tr("key")` — 无参翻译
- `i18n::tr_args("key", &[("param".into(), val)])` — 带参翻译
- `i18n::switch("zh-CN")` — 语言切换
- `i18n::init(Some("zh-CN"))` — 启动初始化

**新增 UI 文本时**：
1. 在 `locales/en/main.ftl` 中新增英文 key（en 是 fallback，语法参考 Fluent）
2. 在 `locales/zh-CN/main.ftl` 中新增对应的中文 key
3. 代码中调用 `i18n::tr("key")` 或 `i18n::tr_args("key", args)`
4. 组件中需 `hooks.use_atom(&LANG_VERSION)` 订阅语言变更以触发重渲染

**Key 命名规范**：
- 命令相关：`command-<name>-description`、`<name>-<status>`
- 面板相关：`panel-title-<name>`、`panel-desc-<name>`、`panel-<name>-<element>`
- 弹窗相关：`popup-<name>-<element>`
- 状态栏：`statusbar-<element>`
- 欢迎/向导：`welcome-<element>`、`setup-<element>`
- 通用提示：`common-<element>`（如 `common-empty`、`common-esc-close`、`common-nav-enter-close`）
- 参数用 `{ $param }` 语法，禁止拼接字符串生成 key

**修改已有 UI 文本时**：若字符串已被硬编码在代码中，替换为 `i18n::tr()` 调用，同步新增 FTL key。

### 测试规范
详见 `docs/design/testing-standards.md`。以下为速查摘要。

#### 存放位置
| 类型 | 位置 | 条件 |
|------|------|------|
| 单元测试 | 同目录 `_test.rs` | 测试代码 ≥ 30 行 |
| 单元测试 | 同文件 `#[cfg(test)] mod tests` | 测试代码 < 30 行 |
| 集成测试 | crate 根 `tests/` | 跨模块端到端，只访问 `pub` API |

#### 优先级
- **P0（必须测）**：serde roundtrip / 事件映射 / 纯逻辑函数 / 工具错误路径 / 中间件链顺序 / CLI 配置解析
- **P1（应该测）**：复杂状态机 / 协议编解码 / 异步通道 / 安全敏感 / Prompt 构建
- **不测**：TUI render body / 外部 API 调用 / 纯样板 getter / `side-projects/`

#### 风格
- **命名**：`test_<对象>_<场景>`（如 `test_serde_roundtrip`、`test_edit_file_old_string_not_found`）
- **结构**：Arrange-Act-Assert 三段，**段间无空行**
- **注释/断言**：中文
- **一条断言法则**：每个 test 验证一个场景；多条 assert 必须验证同一场景的不同侧面
- **异步测试**：`#[tokio::test]`
- **全局状态测试**：`#[serial]` 或 `Mutex<()>` 加锁

#### 质量标准
1. **确定性**：无随机数、无时间依赖、无外部网络
2. **错误路径**：正常路径 + ≥ 1 条错误路径，assert 错误**类型/消息内容**而非仅 `is_err()`
3. **精确断言**：`assert!(err.to_string().contains("not found"))` 而非 `assert!(result.is_err())`
4. **独立可运行**：`cargo test -p <crate> --lib -- <test_name>` 单独通过

#### Mock 与 Fixture
- **`make_` 前缀**工厂函数 + 手写 trait impl，禁止 `mockall` / `Mock struct`
- **不共享**：所有 mock 在测试文件内部局部定义，不设共享 `test_helpers` 模块
- 常用依赖：`tempfile::TempDir`（文件隔离）/ `serial_test`（独占资源）/ `temp-env`（环境变量）

#### 回归测试
标注 `/// [回归测试]` + 历史背景（哪个 bug / 哪次修复），参考 `peri-agent/src/agent/events_v2.rs:1128`。

#### 新增功能 Checklist
- [ ] 新增数据结构（含 serde）→ serde roundtrip + 不完全 JSON 测试
- [ ] 新增 `ExecutorEvent` 变体 → `mapper_test.rs` 增加映射测试
- [ ] 新增 Core 工具 → `core_tools_test.rs` 同步
- [ ] 新增中间件 → before/after_agent + before/after_tool 各 ≥ 1 条
- [ ] 文件系统工具 → 错误路径（not found / ambiguous / permission / not unique）
- [ ] 回归修复 → `/// [回归测试]` 注释 + 历史背景

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

### Langfuse 监控 v2
- **trace_id = turn_id**：tracer.new() 由 caller 传入 turn_id，禁止自生成。trace_id 不可变。
- **sampled=false 时 silently no-op**：每个 on_* 入口检查 sampling，未采样时直接返回。caller 不感知。
- **新增 ExecutorEvent 变体**：必须同步 (1) peri-acp/event/mapper.rs (2) peri-tui/kit/acp_events.rs (3) variant_coverage_test.rs，缺一会漏掉监控数据。
- **ErrorSpan 兜底**：错误 turn 强制发 ErrorSpan 挂同 turn（trace_id = turn_id，不破坏契约）。
- **子对象方法签名禁止接收 `&mut LangfuseTracer`**：否则破坏 disjoint borrow。
- **ToolBatch per-act flush**：ToolBatch 在 Act stage 结束时自动 flush（`on_stage_end`），避免所有工具堆在第一个 Act 下。单个工具以 `ObservationCreate` + `ObservationType::Tool` 上报，含 input/output/end_time/level。batch span 的 parent 在首次 `on_tool_start` 时捕获 stage span_id（时序安全）。

## 任务入口矩阵

| 任务 | 入口文件 | 注意事项 |
|------|---------|----------|
| 新增/删除 Core 工具 | `tool_search/core_tools.rs:38` 的 `CORE_TOOLS` 常量 | 同步 6 处：prompt §05、HITL 审批列表、event/mapper、tool_display、core_tools_test、GitAttribution |
| 新增工具别名 | override `BaseTool::aliases() → &["alias1", ...]` | 工具自声明别名，由 `resolve_tool()` 统一解析（大小写无关），无需修改集中式常量表。别让工具变成"隐性第二名字"——只设 LLM 可能输出的同义词（如 Bash→"Shell", Read→"reading", Agent→"task"） |
| 新增中间件 | `peri-acp/src/agent/builder.rs:490` | 15+5 固定顺序，禁止重排 |
| 改 LLM Provider 调用 | `peri-agent/src/llm/{openai,anthropic}/invoke.rs` | System hoist 规则：禁止 `BaseMessage::system()` 中途注入 |
| 新增 TUI 面板 | `peri-tui/src/kit/panels/` → `app/panel_types.rs:7` 的 `PanelKind` | 用 `panel_shell!` 宏 + `MutexGroup` 分组 |
| 改 Theme Panel | `peri-tui/src/kit/panels/theme.rs` | 主题按 `ThemeMode` 自动分为 Dark/Light 两 tab，`Tab` 切换分类，`↑/↓` 导航，`Enter` 应用+持久化，`Esc` 恢复原主题。选中 tab 用反色（accent 底色+surface 字色），禁止 `[ ]` 包裹。面板内禁止单字母快捷键（j/k/q 等），仅允许方向键 + Tab + Esc + Enter |
| 改 系统提示词 | `peri-acp/prompts/sections/`（14 个 .md 段落） | 静态段结构不可变（破坏 prompt cache） |
| 改 TUI 渲染 | `message_area/`（主渲染，子模块 mod/render/selection/scroll/footer/props）+ `acp_bridge.rs`（事件→状态）+ `acp_events.rs`（push_view_models） | VIEW_MODELS 是唯一数据源 |
| 改 SubAgent | `peri-middlewares/src/subagent/`（工具/构建器/spawner/v2_bridge） | frozen 数据必须从 main agent 透传 |
| 改 MCP 配置 | `peri-middlewares/src/mcp/`（initialize/reconnect）+ `~/.peri/settings.json` | 三层合并：全局→插件→项目 `.mcp.json` |
| 改 Plugin 系统 | `peri-middlewares/src/plugin/`（installer/marketplace/config） | 兼容 Claude Code 生态 |
| 改 Skills | `peri-middlewares/src/skills/`（扫描/叶子语义）+ `skills/builtin/`（编译期嵌入） | 搜索顺序：用户→项目→插件→Builtin |
| 改 Langfuse 监控 | `peri-acp/src/langfuse/tracer/`（9 子对象 + 主 struct：compact/event_builder/generation/middleware/sampling/stages/subagent/tool_batch/usage） + `langfuse-client/`（数据结构） + `peri-acp/src/session/executor_helpers.rs::forward_langfuse_event`（路由） | trace_id = turn_id 契约；新增 ExecutorEvent 必须扩 mapper_test + variant_coverage_test；sampled=false 时 tracer silently no-op |
