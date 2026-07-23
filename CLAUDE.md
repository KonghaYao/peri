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
  Compact（ContextBudget: 0.75 micro / 0.95 full）
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
三层：Core（13，始终可见）/ Meta（2，SearchExtraTools/ExecuteExtraTool）/ Deferred（Cron/MCP/LspTool 等）

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
- `cargo test --workspace --doc`：doc test（`cargo build`/`check`/`clippy` 不编译 doc test，lefthook 也不跑，需显式验证）
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
| 单元测试 | 同文件 `#[cfg(test)] mod tests` |  永远不允许文件内联测试 |
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
`~/.peri/settings.json` env 字段注入。Provider：`ANTHROPIC_*`/`OPENAI_*`。行为：`DISABLE_COMPACT`。权限：`PermissionMode`（通过 `--approve`/`--permission-mode`/`--skip-permissions` CLI 或 `Shift+Tab` 切换）。遥测：`LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_BASE_URL`（必填/可选，`from_env()` 静默启用）。可选：`LANGFUSE_TRACE_SAMPLING`（0.0-1.0）、`LANGFUSE_USER_ID`（自定义 user 维度，仅环境变量，不支持 settings.json）。

### Theme
颜色从 `peri-theme` 三 Atom（`THEME_ATOM`/`PALETTE_ATOM`/`PERI_COLORS_ATOM`）获取，禁止硬编码色值。`#[component]` 用 `hooks.use_atom`，非 component 两步绑定防悬垂引用。

### Workflow 排障
"0 agents, 0 tool calls"：确认 `npx --version` 或 `bun --version` 可用 → 重启。

## E2E 测试（`e2e/`）

基于 [tui-tester](https://github.com/KonghaYao/tui-tester)（tmux 黑盒方案），TypeScript + vitest。

### 架构

```
e2e/
├── tui-tester/          # Terminal E2E 测试框架（TmuxTester + SnapshotManager）
├── helpers/
│   ├── peri.ts          # launchPeri() / sendPrompt() / takePeriSnapshot()
│   ├── recorder.ts      # index.jsonl 录制（writeAnsiSnapshot / appendToIndex / updateJudgeResult）
│   └── judge.ts         # LLM-as-Judge（ANSI → OpenAI → 结构化检查清单）
├── scripts/
│   └── generate-report.ts  # 单文件 HTML 报告（unpkg ansi_up CDN）
├── tests/
│   ├── setup.ts            # 全局 setup（dotenv 加载 / tmux 检查清理 / testName 注入）
│   └── smoke/              # 冒烟测试
│   └── scenarios/          # 场景测试（流式+工具交错 / AskUserQuestion / Goal 续跑）
├── recordings/             # 运行时生成（gitignored）
│   ├── *.txt               # 纯文本快照（默认，人类可读）
│   ├── *.ansi              # ANSI 原始屏幕（recorderConfig.ansi=true 时生成）
│   └── index.jsonl         # 录制索引
├── .env.example            # Judge 环境变量模板
├── .env                    # 实际配置（gitignored）
└── package.json
```

### 数据流

```
TmuxTester.start() → tmux new-session → dev.sh → cargo run -p peri-tui
    ↓
sendPrompt("hello") → tmux send-keys → crossterm KeyEvent → ratatui-kit
    ↓
takePeriSnapshot() → tmux capture-pane -p -e → ANSI 文件 + index.jsonl
    ↓
judge({ansiRaw, criteria}) → OpenAI chat.completions → JudgeResult {pass, checks[]}
    ↓
generate-report.ts → 读 index.jsonl → 单文件 HTML（ansi_up 渲染）
```

### 关键决策

- **纯黑盒**：不测试 ratatui-kit 组件内部，只测 tmux 下的真实 TUI 行为
- **每测试重启 peri**：避免跨测试 session 状态污染（`afterEach` 清理 tmux session）
- **LLM judge 是兜底**：日常用字符串匹配，复杂场景才用 judge；judge 失败不阻断测试
- **录制 = 独立存储**：不与 vitest reporter 耦合，ANSI 文件 + index.jsonl 自包含
- **单文件 HTML 报告**：unpkg CDN 加载 ansi_up，所有数据内联
- **tui-tester 保持专一**：LLM judge / HTML 报告 / peri helper 都在 e2e/ 层，不改 tui-tester

### 命令

- `npx vitest run tests/<path>.test.ts`：**始终跑单个测试**——全量跑耗时 200s+ 且消耗 LLM Judge API token
- `npm run test`：全量（仅发布前使用）
- `npm run report`：手动生成 HTML 报告
- `npx tsx scripts/generate-report.ts --watch`：监听 index.jsonl 变化自动刷新报告

> **详细开发指南见 `e2e/CLAUDE.md`**（含常见陷阱：按键名、Judge 正向断言、面板交互等）

### 环境变量（`.env`）

| 变量 | 必填 | 说明 |
|------|------|------|
| `OPENAI_API_KEY` | 是 | Judge 调用的 API key |
| `OPENAI_BASE_URL` | 否 | API 端点（默认 OpenAI） |
| `JUDGE_MODEL` | 否 | Judge 模型（默认 gpt-4.1-mini） |

### 断言模式

1. **字符串匹配**（日常）：`expect(screen.text).toContain("hello")`
2. **LLM judge**（复杂场景）：
```ts
const result = await judge({
  ansiRaw: capture.raw,
  criteria: ["消息区应有回复内容", "状态栏应显示上下文用量"]
});
// result.pass: boolean, result.checks: [{criterion, pass, detail}]
```

## 陷阱速查

### Agent 循环
- **drain_for_end**：executor 末尾禁止，消息物理丢失。idle 续跑由 TUI 负责
- **await_wake**：`run_session_loop` 末尾禁止，stdio 收不到响应
- **before_tool/after_tool**：不读 `state.messages()`，`tool_dispatch.rs` 延迟写入
- **AgentEvent 变体**：新增需同步 `map_executor_event`（peri-acp/event + peri-tui/acp_events）
- **Interrupted/Error vs Done 互斥**：前者 `request_rebuild()` + `reconcile_already_done=true`
- **ACP 通知覆盖度**：所有事件（含 AgentDone→TurnDone）必须完整转发，遗漏→UI 卡死。架构迁移时新增事件通道必须同步更新 notifier 分发，否则静默丢弃（详见 spec/global/domains/agent.md#issue_2026-07-07-subagent-group-header-shows-agent-instead-of-task-description）
- **PromptFeatures**：`detect(permission_mode)` 根据当前权限模式决定功能开关，未 frozen
- **Immediate 命令**：绕过 event pump，必须手动 `sink.push_done()`
- **中途纠正消息**：用 `BaseMessage::human()`，禁止 `BaseMessage::system()`（invoke.rs 会 hoist 污染 frozen prompt）
- **诊断优先于修复**：根因未被日志/复现步骤/代码证据定位前，禁止超过 20 行的代码修改。添加诊断日志优先于修改业务逻辑
- **3 次尝试上限**：同一 bug 修复超过 3 次仍失败时，禁止继续猜测式修复。要求用户提供运行时数据（RUST_LOG=debug 等）后再动手
- **工具超时差异化**：超时策略按工具类型差异化——BaseTool::timeout() 自声明，快工具 120s，长运行工具（Agent/Bash/Workflow）返回 None 自管。禁止一刀切硬编码（[issue](spec/archive-issues/2026-07-13-agent-tool-300s-timeout-interrupts-normal-tasks.md)）
- **事件路径统一**：新增事件变体走 EventBus 单一入口，避免分散 ExecutorEvent 直接构造。SubAgent/LLM 流式/斜杠命令等路径须进入 EventBus（[issue](spec/archive-issues/2026-07-16-eventbus-unified-emission.md)）
- **序列化确定性**：跨进程/跨请求复用的序列化内容必须保证顺序稳定——`HashMap.values()` 迭代顺序不确定，tools 数组顺序变化会破坏 Anthropic 前缀缓存。应使用 BTreeMap 或固定收集顺序（详见 spec/global/domains/agent.md#issue_2026-07-18-tools-hashmap-order-breaks-prompt-cache）
- **Goal 续跑**：注入控制消息时需理解 MessageKind 语义——Info（不唤醒）vs Defer（唤醒续跑）。ActOutput 须保留 `block_continue` 字段，否则中间件设置的续跑信号在 act 阶段被吞掉（详见 spec/global/domains/agent.md#issue_2026-07-15-goal-continuation-loop-broken-in-v2）
- **Compact 标记持久化**：持久化标记（truncated/excluded）须独立于内容字段存储和恢复。cached_context 缓存命中时跳过 DB 查询会漏掉标记；v2 路径 `persist_tx=None` 导致 compact flags 全丢（详见 spec/global/domains/agent.md#issue_2026-07-17-compact-flags-lost-on-session-restore）（详见 spec/global/domains/agent.md#issue_2026-07-18-compact-effect-lost-between-prompts-v2）
- **or_insert_with 复用陷阱**：`or_insert_with` 不适合需要每 turn 重建的有状态对象（含 channel/sender）。SubAgentTool 实例的 `event_tx` 在第二 turn 时可能已被 close，所有 SubagentStarted 事件静默丢弃（详见 spec/global/domains/agent.md#issue_2026-07-13-sync-agent-tool-cards-not-showing）
- **AskUserQuestion 参数序列化**：`questions` 数组参数用 Python dict 字面量格式（单引号键值），避免嵌套 JSON 双引号转义不一致。如遇 `missing field 'questions'` 错误，优先检查 JSON 引号转义而非字段缺失。示例：`{"questions": [{"question": "...", "header": "...", "options": [{"label": "..."}]}]}`（详见 spec/archive-issues/2026-07-19-ask-user-question-param-parse-fail.md）

### ACP/TUI 分层
- **execute_prompt**：Agent 构建统一入口，禁止 TUI 直连运行时
- **SubAgent frozen**：必须复用 main agent frozen 数据，禁止重新读盘
- **add_message vs prepend_message**：非 System 消息用 `add_message`（尾部追加）
- **`_meta` key**：ACP SDK 序列化 key 是 `"_meta"` 非 `"meta"`。`is_session_replay` 检测用四级 fallback（`_meta`→`meta`→`content._meta`→`content.meta`）
- **session replay DTO**：复用正常流式路径数据结构，不要发明 replay 专用变体
- **ACP 协议字段优先**：数据流设计时先查 ACP 协议已有字段（如 `StateSnapshotMeta.budget_pct`、goal_state `block_continue`），禁止自己从 raw 数据重新计算
- **工具别名**：通过 `BaseTool::aliases()` trait 方法自声明，禁止重复注册或集中式静态表。别名仅用于 LLM 可能输出的同义词（如 Bash→"Shell"）。工具包装/过滤层必须完整透传所有 trait 方法（详见 spec/global/domains/agent.md#issue_2026-07-16-subagent-tool-alias-not-resolved）
- **面板配置必须推送**：任何修改全局配置的面板操作后必须：save 持久化 + client.update_config() 推送 + invalidate pool + 重建 provider。workflow/SubAgent 的 provider 须共享 Arc（[issue](spec/archive-issues/2026-07-16-model-login-switch-not-effective-until-restart.md)）

### PeriCaps 能力协商（标准模式）

**PeriCaps** 是 ACP 自定义通知通道的能力标志集（`peri-acp-types/src/peri_caps.rs`），10 个 bool 字段：`token_stats`、`skill_names`、`replay`、`source_agent_id`、`context_usage`、`agent_event`、`agent_event_done`、`unstable_event`、`prediction`、`hitl_pending`。

存储于 `SessionManager.caps_registry: Arc<DashMap<String, PeriCaps>>`，per-session 粒度。

**生命周期**：
```
initialize（stdio）或 跳过（TUI MpscTransport）
    ↓
pending_caps: Mutex<Option<PeriCaps>>  // 暂存
    ↓
session/new → consume or default → caps_registry[session_id]
    ↓
push_event / push_done / replay / notify 等发送点读取 → if caps.xxx { ... }
```

**两条传输路径的默认值差异**：

| 路径 | initialize | session/new 默认 | 原因 |
|------|:---------:|:--------------:|------|
| Stdio（IDE） | ✅ 有 | `consume_pending_caps` → 协商值 | 外部 client 显式声明能力 |
| MpscTransport（TUI 内部） | ❌ 无 | `PeriCaps::all_enabled()` | TUI 需要接收所有自定义事件 |

**标准 API**（`SessionManager`）：

| 方法 | 用途 | 调用时机 |
|------|------|---------|
| `set_pending_caps(caps)` | initialize handler 暂存协商值 | initialize |
| `consume_pending_caps(sid) → PeriCaps` | 取走 pending_caps 并写入 registry | session/new（仅 stdio） |
| `get_caps(sid) → PeriCaps` | 只读查询（未注册返回 `default()`=全 false） | 发送点 |
| `ensure_session_caps(sid) → PeriCaps` | **幂等注册**：已有则返回，无则从 pending 取或 fallback `all_enabled()` | session/new/load/resume/fork（**所有路径标准调用**） |
| `caps_registry() → Arc<DashMap<...>>` | 获取 registry 克隆引用（供 TransportEventSink） | session/prompt 启动时 |
| `pending_caps_was_set() → bool` | 判断 initialize 是否已调用 | 路径判断（已弃用，`ensure_session_caps` 内部原子化） |

**关键规则**：
1. **新增 session/new/load/resume/fork 时**：在 `ensure_session()` 后立即调用 `ensure_session_caps()`。**这是标准**——缺少会导致 `push_done()` 读到 `default()`，`agent_event_done` 被抑制，TUI 永久 loading。
2. **stdio 路径 session/new**：继续用 `consume_pending_caps`（因 initialize 保证 pending_caps 非空）。
3. **新增发送点**：`EventSink::push_event`/`push_done`/`push_unstable_event` 入口必须读 caps 做 if-check。
4. **`PeriCaps::default()` = 全 false**：未注册 session 的返回值，静默抑制所有自定义功能。
5. **新增 cap 字段**：同步 (1) `peri_caps.rs` struct 定义 (2) `from_client_meta` 解析 (3) `to_agent_meta` 序列化 (4) `all_enabled()` 工厂 (5) 对应的发送点 if-check。

### TUI 渲染
- **use_* 顺序**：`hooks.use_*` 必须在 `if`/`match`/`return` 之前，顺序/数量变化→`"Hook type mismatch"` panic
- **render body 写 atom**：禁止（含 `use_effect`），`ReactiveMutRef::Drop` 无条件 `wake()` → 自激回路
- **use_state write_no_update**：render body 内 `use_state` 写入必须用 `write_no_update()`，不是 `write()`
- **BRIDGE_RESET_COUNTER**：/clear 和 thread 切换前必须递增，仅 atom 重置不足
- **overlay 空态**：返回 `Positioned(width:0, height:0)`，不要 `View()`/`Fragment` → 白屏
- **事件边界**：消息区只处理鼠标滚轮，编辑区只处理键盘
- **committed push**：所有需时序定位的消息须先 flush current_turn → committed 再 push（flush-then-push 模式）。禁止直接 push committed 绕过 TurnSegment（SystemNote 除外，见下条）（[issue](spec/archive-issues/2026-07-16-system-note-cache-warning-position-wrong.md)）
- **SystemNote 注入**：所有 SystemNote（CompactCompleted/CompactError/AgentExecutionFailed/BudgetWarning/SystemNotification）必须通过 `BridgeState::inject_system_note(text, level)` 统一入口注入，禁止直接 `committed.push_back(TuiSystemNote{...})` 或手工 `push_system_note() + push_view_models + push_acp_state`。`inject_system_note` 封装三步操作，确保 SystemNote 按时序出现在 current_turn 内部。这是第三次同类回归（2026-07-16 → 2026-07-20 → 2026-07-22），新增 handler 时务必遵守此规则（[issue](spec/issues/2026-07-20-cache-warning-systemnote-position-regression.md)）
- **渲染异常优先检查事件注入**：消息区内容跳变/布局异常时，优先排查是否有 SystemNote/cache 警告/其他事件在流中插入了额外内容，再检查布局计算。事件注入是 3 次同类回归的根因——2026-07-18 滚动跳变诊断中 agent 在布局方向浪费 ~90 分钟才找到真正根因（详见 spec/global/domains/tui/tui-rendering.md#issue_2026-07-18-scroll-jump-event-injection）
- **增量缓存 can_reuse**：条件须覆盖 block 类型变更场景——输入前缀可能导致 pulldown-cmark 重解析出不同 block 类型时，缓存必须失效全量重跑（[issue](spec/archive-issues/2026-07-15-markdown-table-raw-text-streaming.md)）
- **面板交互规范**：选中 tab 用反色（accent 底色+surface 字色），禁止 `[ ]` 包裹。面板内禁止单字母快捷键（j/k/q 等），仅允许方向键 + Tab + Esc + Enter。文本输入用 `TextAreaState`，禁止手工键盘事件处理
- **ratatui-kit 迁移回归**：UI 框架迁移需要系统性功能回归清单——状态栏上下文消耗显示和缓存命中率警告在 ratatui-kit 迁移后丢失（详见 spec/global/domains/tui/tui-rendering.md#issue_2026-07-13-statusbar-context-cache-display-regression）
- **Loading 状态**：loading 应由"是否有活跃流式 agent"决定，而非"是否有 SubagentStopped 事件"。bg agent 完成时 SubagentStopped 无条件设 `phase=PromptRunning` 覆盖了之前 TurnDone 清除的 loading（详见 spec/global/domains/tui/tui-rendering.md#issue_2026-07-13-main-agent-done-loading-persists-bg-still-running）
- **滚动性能**：高频事件 handler 内状态修改合并为单次 `write_no_update()`，不触发多次原子通知→render loop。ScrollThrottle 16ms 节流，tmux 下 PTY 开销放大（详见 spec/global/domains/tui/tui-rendering.md#issue_2026-07-05-scroll-performance-lag）
- **ESC 事件优先级**：面板/弹窗的 ESC 处理应使用 `EventPriority::High`，确保先于全局 handler 执行。否则全局 ESC 截断面板 handler，`cancel_ask_user()` 不调用→agent 永久挂起（详见 spec/global/domains/tui/tui-popups.md#issue_2026-07-13-ask-user-esc-freeze-reject）
- **History 恢复滚动**：初始加载/恢复场景需"强制吸底窗口"（如 20 帧=333ms），覆盖所有批次到达。`scroll_to_bottom` 过早执行时 ScrollViewState.size=None→offset 无效，后续因 proximity guard 永不滚底（详见 spec/global/domains/tui/tui-rendering.md#issue_2026-07-11-history-replay-scroll-too-early）

### SubAgent / Worktree
- **coder cwd**：不遵守 `Agent(cwd=...)`，prompt 中必须用绝对路径。push 前 `git diff --stat` 确认
- **文件白名单**：coding prompt 必须列文件白名单 + `DO NOT modify:` 禁止清单。
- **Agent 任务粒度**：单次 Agent 调用最多处理 8 个文件，超过则拆分批次
- **批量机械替换用 perl**：超过 10 个文件的模式替换（重命名、import 路径等），用 `perl -i -pe`/`find ... -exec sed` 脚本，禁止逐个 Edit 或委托 coder subagent
- **bg agent 构建检查**：后台 agent 完成后必须 `cargo build` 验证。失败则 `git checkout -- <modified files>` 恢复文件，切换到手动模式
- **AgentResult 禁止轮询**：后台任务结果通过 system-reminder 自动推送。禁止调用 `AgentResult()` 轮询——浪费 token 且结果会重复推送
- **禁止 shell sleep 等待异步结果**：后台 subagent / workflow 结果通过 system-reminder 自动推送并唤醒 agent。禁止用 `bash sleep N`/`timeout`/轮询循环等待——sleep 会错过通知窗口、浪费 token。派发异步任务后立即停止，等系统唤醒即可
- **Workflow inline script 反引号冲突**：JS 模板字面量（`` `...` ``）会与 workflow inline script 的字符串定界符冲突。在 inline script 中避免使用模板字面量，改用字符串拼接（`'str' + var`）

### Rust / 编码
- **rustfmt import**：`use crate::module::*` 通配导入排在单类型之前；跨 crate 同理
- **CJK 截断**：用 `chars().take(N)`，`&s[..N]` 对中文 panic
- **u16 坐标**：用 `saturating_add`/`saturating_sub`，禁止裸 `+`/`-`
- **RwLockReadGuard Send**：不是 Send，async 跨 `.await` 用 `parking_lot::RwLock`
- **剪贴板**：阻塞系统 I/O 用 `std::thread::spawn` 独立线程
- **Paste 事件**：`Event::Paste` 独立于 key event，需 BracketedPaste；事件过滤禁止 `if let Event::Key` 独占模式丢弃 Paste（[issue](spec/archive-issues/2026-07-15-setup-wizard-no-paste-login-no-edit.md)）
- **鼠标 Drag 事件**：高频 Drag(Left) 必须在入口处及早 Ignored（与 Moved 同级过滤），否则穿透触发 state 读写 → 组件重渲染 → CPU 暴涨（[issue](spec/archive-issues/2026-07-11-message-area-mouse-selection-regression.md)）
- **Theme 悬垂引用**：`&THEME_ATOM.state().read().xxx` 崩，须两步绑定；`theme_def.semantic` 不能直接访问，须 `theme_def.read().semantic`
- **Edit 前必 Read**：连续编辑同一文件时，每次 Edit 前必须 Read 确认 old_string 匹配当前文件状态。同文件修改超过 3 处，用 Write 整体重写替代逐块 Edit
- **首次访问 Glob 确认路径**：不确定文件确切位置时，先用 `Glob("**/<filename>")` 确认完整路径再 Read/Edit。避免凭记忆猜测路径结构（如 `acp_client.rs` vs `src/acp_client/client.rs`、已删除文件仍被引用）
- **跨项目操作确认 pwd**：修改非当前项目目录的文件前，显式确认 `pwd`。若发现误操作，用 `git checkout --` 恢复并切换正确目录
- **commit 消息特殊字符**：含 `{}`、`\`` 等 shell 特殊字符时，用 `git commit -F /tmp/msg.txt` 而非 `-m`。提交前 `git diff --cached --stat` 确认 scope
- **cargo fmt 参数**：`cargo fmt -- -p peri-tui`（注意 `--`），非 `cargo fmt -p peri-tui`
- **let-chains + rustfmt 不兼容**：peri-tui/peri-theme edition=2024 的 let chains 语法导致 `cargo fmt` 报错。提交时若卡 fmt 可 `--no-verify` 跳过
- **doc test 盲区**：`cargo build`/`check`/`clippy` 不编译文档中的 ` ``` ` 代码块，lefthook 也不跑。修改 doc comment（含 ASCII 图）后必须 `cargo test -p <crate> --doc` 验证。非代码块（架构图、示意图）用 ` ```text` 标记，避免被当作 Rust 编译

### Langfuse 监控 v2
- **trace_id = turn_id**：tracer.new() 由 caller 传入 turn_id，禁止自生成。trace_id 不可变。
- **sampled=false 时 silently no-op**：每个 on_* 入口检查 sampling，未采样时直接返回。caller 不感知。
- **user_id 定制**：通过 `LANGFUSE_USER_ID` 环境变量设置（仅环境变量，不读 settings.json）。有值时 `on_turn_start` 先发 TraceCreate 设置 user 维度，无值则为 None。
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
