# TUI Chat 与 Tool Activity Workbench 目标设计

> 状态：已批准目标设计（当前实现基线只用于解释兼容边界；进度见对应 active issue）
> 范围：`peri-tui` 的 chat transcript、全部 Agent tool call、Tool Inspector、交互弹窗、滚动、focus、selection 与鼠标事件
> 上游边界：`Agent → ACP → TUI`；涉及新增事件或详情请求时遵守 `ARC-EVENT-001` 与 `ARC-BOUNDARY-001`
> 视觉基础：延续现有连续 transcript、统一水平网格、语义 token 与响应式断点
> 非目标：在本文中修改 Rust 实现、构建完整 IDE、让鼠标成为必需输入、把任意原始 payload 无条件发送到客户端

## 1. 文档定位与规范语言

本文替代“所有工具共用一张通用卡片”的粗粒度设计，给出完整的工具展示与交互目标。它保留原设计的内容优先、连续时间轴、默认安静和键鼠等价原则，在此基础上增加：

1. 面向每个现有工具及动态工具的语义化展示；
2. `Inline → Preview/Expanded → Inspector` 的渐进披露；
3. 仅用于阻塞决策的 Modal；
4. 统一的 semantic hit region、hover、press、drag 与 focus 模型；
5. transcript、Inspector、Panel、Modal 的区域化滚动所有权；
6. 大输出、流式日志、敏感内容和动态 MCP/plugin 工具的安全 fallback；
7. 可重复验证的验收契约。

本文使用以下标签区分事实与目标：

- **当前基线**：仓库代码已经实现、可直接验证的行为；
- **目标规范**：本次 redesign 完成后必须满足的行为；
- **协议依赖**：仅修改 TUI 无法完成，必须扩展 Agent/ACP 数据链路；
- **兼容回放**：历史 session 缺少新字段时必须提供的降级行为。

标签是逐项契约，不只是章节说明。混合事实表必须使用“标签”列；同一行同时涉及现状、目标、协议与历史数据时，应拆成四列或四条断言。纯目标章节则在规则集合或表格前显式标注 **目标规范**；不得依靠文档标题或上下文暗示分类。

`必须/不得` 是验收约束；`应` 是默认行为，偏离时需在 active spec 中记录原因。代码与本文冲突时，以代码和契约测试为当前事实；本文描述的是目标，不得反向把尚未实现的能力写成现状。

## 2. 设计目标与非目标

**目标规范**：本节定义 redesign 的产品目标、原则和明确非目标。

任何时刻，用户都应能回答：

- Agent 正在做什么，作用对象是什么；
- 哪些调用仍在运行、等待审批、成功、失败、被拒绝、取消或超时；
- 结果是否完整，是否还有可查看的 diff、log、结构化 output 或嵌套 Agent 详情；
- 当前滚动和键盘输入会作用于 transcript、Inspector、Panel 还是 Modal；
- 哪个鼠标目标可点击，点击后会发生什么；
- 哪些内容已被截断或脱敏。

### 2.1 核心原则

- **正文优先，过程可审计**：最终回答保持最高阅读优先级；每次工具调用都保留一条可回放的审计记录。
- **连续时间轴，不做卡片墙**：所有消息与 tool activity 共用左对齐网格；禁止给每次调用绘制完整矩形边框。
- **默认安静，按需深入**：成功调用收束为一行；错误、待交互与安全警告保持可见；长内容进入 Inspector。
- **一条调用，一个稳定 entry**：事件层同一 `tool_call_id`（进入 TUI 后映射为 `tool_id`）的生命周期更新原地修改，不按 event/chunk 追加重复卡片。
- **鼠标是加速器，不是能力边界**：所有语义动作都有键盘路径；hover 不承载独占信息。
- **安全摘要，不 dump JSON**：Inline 永不显示任意原始 JSON；unknown tool 也必须走脱敏和字段白名单。
- **状态不只靠颜色**：symbol、状态词、modifier 与颜色共同表达状态。
- **布局不抖动**：spinner、duration、hover action 和 streaming 更新不得改变 header 的主锚点。
- **区域拥有事件**：鼠标按 z-order 与命中区域路由；键盘按 `FocusOwner` 路由；不再依赖多个 handler 的注册偶然顺序。
- **详情不是决策**：普通详情使用非模态 Inspector；Modal 只用于审批、提问、OAuth、不可逆确认等必须先响应的流程。

### 2.2 明确非目标

- 不创建永久三栏 IDE、可拖动分割线或每张卡片内的独立 scrollbar；
- 不支持 hover-only tooltip、鼠标手势速度、像素坐标或长按语义；
- 不允许双击批准、整窗批准、点击历史调用直接重跑或自动打开外部程序；
- 不在主 transcript 铺开完整 SubAgent transcript、Workflow journal、stdout 或 MCP resource；
- 不为每个未来 plugin 工具编写专用组件；未知工具必须由通用安全 presenter 覆盖；
- 不在 TUI 建立绕过 ACP 的详情读取、取消或执行通路。

## 3. 当前实现基线与差距

### 3.1 工具事件与渲染链路

**当前基线**：当前 canonical 路径为：

```text
RenderEvent::ToolStarted / ToolEnded
  → event_v2 协议序列化面
  → peri-acp event mapper / session update
  → peri-tui acp_notifier
  → BridgeState / CurrentTurn
  → VIEW_MODELS
  → message_area viewport renderer
```

关键事实源：

- 工具契约：`peri-acp-types/src/tools.rs::BaseTool`；
- 工具执行与事件发射：`peri-agent/src/agent/stages/tool_dispatch.rs`；
- ACP 映射：`peri-acp/src/event/mapper.rs`；
- TUI 解码：`peri-tui/src/kit/acp_notifier.rs`；
- TUI 累积：`peri-tui/src/kit/acp_events/tool.rs`、`peri-tui/src/kit/acp_types.rs`；
- ViewModel：`peri-tui/src/kit/tui_render_unit.rs::TuiToolCard`；
- 展示语义：`peri-tui/src/kit/tool_semantics.rs`、`peri-tui/src/truncate.rs`；
- 渲染与交互：`peri-tui/src/kit/message_area/`。

### 3.2 已实现能力

**当前基线**：

| 主题 | 当前基线 |
| --- | --- |
| Transcript | 连续左对齐时间轴、统一 `GridSpec`、视口裁剪、分片渲染缓存、响应式断点已实现 |
| Tool card | 统一 `TuiToolCard`；Running→Preview、Completed→Collapsed、Error→Expanded summary |
| 专属展示 | `SkillTool`/旧名 `Skill` 与 `TodoWrite` 有专属 presentation；`Bash`、`Edit`、`Write` 在通用 renderer 内有局部特判 |
| Diff | Edit/Write 尝试解析 unified diff，失败时回退为变更摘要；完整 diff 是否可达取决于上游 output |
| SubAgent | 主时间轴有 `TuiSubAgentGroup`，并已有 SubAgent 详情 Panel |
| Workflow | 已有 Workflow Panel；后台状态经每 2 秒一次的 `workflow/list_runs` 轮询更新（`WorkflowProgress` 事件在源码树零构造点，见 §3.3 gap #25） |
| HITL / AskUser | 已有 durable interaction 数据、HITL Popup 与 AskUser Panel |
| Panel / Popup | Panel 是消息区与输入区之间的底部区域；Popup 是居中清屏矩形；已有滚动、area tracking 与部分 click-as-enter 设施 |
| MessageArea 鼠标 | 已支持滚轮、scrollbar click/drag、文本 Down/Drag/Up 选区、entry header 点击与显式 interaction/copy 热区 |
| Hover | transcript 当前忽略 `MouseEventKind::Moved`；已知 hover 仅在局部弹出组件中存在 |
| Follow | `FollowBottom`、BrowseHistory、新输出提示、滚动节流与停手 flush 已有基础 |

### 3.3 目标设计必须解决的差距

**当前基线**：以下均是当前源码中已经存在、目标设计必须修复或明确兼容的差距。

1. **工具语义过少**：除 Skill/Todo 外，大多数调用走 Generic，显示名、摘要、预览与详情没有统一的 family presenter。
2. **动态工具 fallback 不安全**：`summarize_input` 的 unknown 分支会回退到 object 的首个字段；该字段可能无意义，也可能是 secret。
3. **执行身份未完全进入 tool event**：resolver 已区分模型请求的 raw outer call、canonical policy call、实际 `target` 与 wrapper；HITL 使用 policy/effective 投影，但 `ToolStarted/ToolEnded` 仍发送 raw outer name/input。来源轴已部分落地（`_peri.sourceAgentId` 无条件注入并解码为 `agent_id`，见 §8.2），但 TUI 仍不能仅凭 tool event 可靠还原 alias canonical name、`ExecuteExtraTool` 的 resolved target、归一化参数或审批后编辑过的 effective input。
4. **结构化 output 会丢失**：ACP 可发送 JSON `rawOutput`，TUI 当前只接受字符串；object/array 会被转换为空摘要。
5. **完整详情不可保证**：TUI 内部 `TuiToolEnded` 只保留 `output_summary`；运行中没有 stdout/stderr/progress chunk 协议。
6. **Panel 不是 Inspector**：当前 Panel 是全宽 bottom drawer，并通过 `InputArea(hidden: panel_open)` 隐藏 composer；它不支持 Wide 下 transcript 与详情并列滚动。
7. **鼠标命中分散**：entry、copy、interaction、scrollbar 等矩形由不同路径维护，缺少统一 frame generation、z-order 和 semantic target。
8. **没有 transcript hover**：用户无法通过 hover 得到稳定 affordance；同时不能以增加高频重绘为代价直接监听每次 Moved。
9. **滚动所有权仍是全局遮挡模型**：`mouse_router::is_occluded()` 将任意 Panel/Popup 视为遮挡全部背景，无法支持 docked Inspector 与 transcript 并列操作。
10. **审批鼠标路径不安全**：当前 `HitlPopup` 将整窗区域内的左键 `Down` 直接解释为批准；目标实现必须首先移除此行为。
11. **稳定规范滞后**：`TUI-EVENT-001` 仍写“消息区只消费鼠标滚轮”，但当前代码已经消费点击、拖拽、scrollbar 与消息区键位。实现本目标前必须同步修订标准与 `peri-tui/CLAUDE.md`。
12. **artifact 可见性注释冲突**：`ArtifactTool::is_direct()` 当前返回 `true`，文件注释仍称 deferred。展示设计不依赖该分类，但实现计划应单独消除事实冲突。
13. **interaction 完成后仍保持展开**：当前 `fold_for_status(Interaction, Completed)` 返回 `Expanded`；目标状态改为有明确答案摘要的 `Collapsed result`，完整问题、选项与回答仍可手动展开。
14. **解析前失败在 live UI 不可见**：空/重复 `tool_call_id`、resolver failure，以及 middleware 试图修改调用 ID/name 时，当前只把错误结果写入 transcript，不发 `ToolStarted/ToolEnded`；同一调用因此可能 live 无 entry、重放后才出现。
15. **审批等待没有稳定 card 关联**：生产审批经标准 ACP `session/request_permission` 发送带 `tool_call_id` 的 `ToolCallUpdate(Pending)`，但 TUI 转成的 `event_data::HitlPending` 只保留 tool name/input，RPC `request_id` 又不同于 `tool_call_id`；现有 card 不能可靠原位进入 `AwaitingApproval`。`RenderEvent::HitlPending` 虽有 `tool_call_id`，当前并非这条生产审批 UI 通路。
16. **审批等待不响应 turn cancel**：HITL broker 最长等待 300 秒，等待本身未与 Agent cancellation token 竞速；取消检查要到 before-tools batch 返回后才执行，故 Modal 可能在 turn 已取消后继续阻塞到响应或超时。
17. **终态语义在 wire 上坍缩**：执行失败、用户拒绝、取消和 tool timeout 最终都只投影成 `ToolEnded { is_error: true }` / ACP `failed`；TUI 当前 `EntryStatus` 仅有 Running/Completed/Error，无法可靠区分 `Denied`、`Cancelled`、`Interrupted` 与 `TimedOut`。
18. **live 与 replay 的 finalized output 分叉**：live `ToolEnded` 在 `run_after_tool`、`error_suggest` 注入和 `output_char_limit` 截断前发射；transcript/replay 读取后处理结果。同一调用可能在 live 与重放显示不同摘要和完整性。截断当前按 `all_tools.get(call_id)` 在 call-id 维度查找；因 middleware 不得修改调用 ID/name（否则整调用拒绝），查找与 `policy_call` 自洽，但按 ID 而非按 name 是脆弱点，若未来引入调用改写将静默失效。另 `output_char_limit` 仅 `Bash` 覆盖为 `Some(10000)`，经 `ArcToolWrapper`（fork 继承）后回落到默认 `None`（见 §4.3）。
19. **turn 边界不能补偿缺失终态**：`RenderEvent::TurnCompleted` 虽携带 `finalized_messages`，但 `peri-acp/src/session/event_sink.rs` 在生产 AgentEvent 路径直接丢弃对应 `ExecutorEvent::TurnCommitted`；TUI 的 `handle_turn_committed` 因而主要是测试/兼容可达的刷新检查点，且即使到达也忽略 `messages_json`。`TurnDone` 缺少 `ToolEnded` 时可能继续 spinner，中断/挂起路径则可能误标 Completed。
20. **end-before-start 与 late end 静默丢失**：live/replay reducer 找不到同 ID 的 running card 时直接 no-op，不创建审计 entry，也不记录 anomaly；TurnDone reset 后到达的迟到 `ToolEnded` 同样消失。
21. **replay 重复和 ID reuse 不收敛**：`ReplayToolStarted` 每次无条件追加 running card；`ReplayToolEnded` 只更新 committed 中第一张同 ID 的 running card。重复 start 或历史跨消息复用同 ID 时，可能留下重复卡和永远 running 的后继卡；第二个 duplicate end 通常只是 no-op。
22. **空 wire ID 在 TUI 会相互覆盖**：Agent dispatch 已阻止 malformed 空/批内重复 ID 发出 live tool event，但 notifier 对异常/旧服务端缺失 `toolCallId` 使用空串；同 turn 多个空 ID 会被 `CurrentTurn::start_tool` 当作同一调用去重或升级。
23. **ToolCount/Progress 载荷被丢弃**：typed event 能解码二者，但 handler 只重推 ACP state，不保留 count/progress payload，因而不能驱动可审计进度 UI。
24. **后台完成事件存在死路径残余**：唯一存活路径是 `bg-task-completed` unstable event → `BG_TASKS/BG_DISPLAY` 短暂通知。`AcpEvent::BackgroundTaskCompleted` 从未被构造：executor 不再经 EventSink 直推该事件，`event_sink` 也无对应映射臂；TUI 的 notifier 解码臂与 handler 只写 tracing 日志，是不可达死代码。后台结果的可见注入实际走 `MessageKind::Defer` 明文回调（见 §4.3 `AgentResult` 限制与 §4.5）。
25. **Workflow 可见状态只有轮询单一数据源**：`WorkflowProgress` 在 ExecutorEvent/AgentEvent 全树零构造点，TUI handler 与 notifier 解码臂是死代码；Workflow Panel 的后台状态只来自每 2 秒一次的 `workflow/list_runs` 轮询，不存在事件驱动的权威 reducer。

## 4. 工具全集与动态边界

本文只把实现 `BaseTool`、能够进入 Agent 工具注册表并产生 tool event 的能力视为工具；progress、tool count、compact、system notification 等事件不是工具。

### 4.1 当前生产 `BaseTool` 身份与暴露矩阵

**当前基线**：下表审计当前生产装配可产生的 `BaseTool` 实例。`Registry key` 是实例进入对应工具 map 时使用的 canonical `name()`；别名不另建 key，只参与 resolver 匹配。`Direct` 表示 `is_direct() == true`，`Deferred` 表示默认或显式 `false`。

| Rust `BaseTool` 类型 | Canonical `name()` / `aliases()` | Registry / exposure | Provider、注册条件与作用域 |
| --- | --- | --- | --- |
| `ReadFileTool` | `Read` / `reading` | key `Read`；Direct | `FilesystemMiddleware`；主 session，且在 parent tools 中可供 SubAgent 继承 |
| `WriteFileTool` | `Write` / — | key `Write`；Direct | `FilesystemMiddleware`；主 session，可按 child allow/disallow 继承 |
| `EditFileTool` | `Edit` / — | key `Edit`；Direct | `FilesystemMiddleware`；主 session，可按 child allow/disallow 继承 |
| `GlobFilesTool` | `Glob` / — | key `Glob`；Direct | `FilesystemMiddleware`；主 session，可按 child allow/disallow 继承 |
| `GrepTool` | `Grep` / — | key `Grep`；Direct | `FilesystemMiddleware`；主 session，可按 child allow/disallow 继承 |
| `FolderOperationsTool` | `folder_operations` / — | key `folder_operations`；Direct | `FilesystemMiddleware`；主 session，可按 child allow/disallow 继承 |
| `BashTool` | `Bash` / `Shell` | key `Bash`；Direct | `TerminalMiddleware`；主 session，可按 child allow/disallow 继承 |
| `WebFetchTool` | `WebFetch` / — | key `WebFetch`；Direct | `WebMiddleware`；主 session，可按 child allow/disallow 继承 |
| `WebSearchTool` | `WebSearch` / — | key `WebSearch`；Direct | `WebMiddleware`；主 session，可按 child allow/disallow 继承 |
| `TodoWriteTool` | `TodoWrite` / — | key `TodoWrite`；Direct | `TodoMiddleware`；主 session；不进入子 agent 工具表（子链 collect_tools 不合并，见 §4.3） |
| `AskUserTool` | `AskUserQuestion` / — | key `AskUserQuestion`；Direct | session 装配时直接插入 shared tools；HITL 不实现 collect_tools，该 key 不被链合并覆盖；使用 interaction broker |
| `SkillTool` | `SkillTool` / — | key `SkillTool`；Direct | `SkillsMiddleware`；catalog 由 session 的 user/global/project/plugin/builtin roots 决定；主 session 独有（子链 Skills 仅经 SkillPreload 注入内容；Workflow Agent 的 `build_tools` 有 project-level 特例，见 §4.3） |
| `DiscoverSkillsTool` | `DiscoverSkillsTool` / — | key `DiscoverSkillsTool`；Direct | `SkillsMiddleware`；与当前 session scan cache 绑定；可见性同 `SkillTool` |
| `SubAgentTool` | `Agent` / `task` | key `Agent`；Direct | `SubAgentMiddleware`；`ChainSlot::SubAgent` 恒在主 session 蓝本中装配；子 agent 创建所需的 frozen/运行时数据经 `SubagentHost` 在创建时注入 |
| `SearchExtraTools` | `SearchExtraTools` / — | key `SearchExtraTools`；Direct meta | `ToolSearchMiddleware`；搜索本 session 的 deferred index；主 session 独有 |
| `ExecuteExtraTool` | `ExecuteExtraTool` / — | key `ExecuteExtraTool`；Direct meta wrapper | `ToolSearchMiddleware`；解析 `tool_name` 后经 resolver 绑定 registry 中的真实 target；主 session 独有 |
| `ArtifactTool` | `artifact` / — | key `artifact`；**Direct** | 由 `ToolSearchMiddleware` 提供；provider provenance 不等于 deferred exposure |
| `CronRegisterTool` | `cron_register` / — | key `cron_register`；Deferred | `CronMiddleware`；主 session 生产链始终注册，缺 session scheduler 时构造临时实例；SubAgent 不继承 |
| `CronListTool` | `cron_list` / — | key `cron_list`；Deferred | 同上；主 session 独有 |
| `CronRemoveTool` | `cron_remove` / — | key `cron_remove`；Deferred | 同上；主 session 独有 |
| `LspTool` | `LSP` / — | key `LSP`；Deferred | `LspMiddleware`；配置并可用的 LSP server 存在时注册 |
| `WorkflowTool` | `Workflow` / — | key `Workflow`；Deferred | `WorkflowMiddleware`；`workflow_executor.is_some()` 时注册，invoke 异步返回 `run_id` |
| `GoalTool` | `goal` / — | key `goal`；Deferred | `GoalMiddleware`；Goal controller 可用时注册 |
| `AgentResultTool` | `AgentResult` / — | key `AgentResult`；Deferred/synthetic | `SubAgentMiddleware`；生产主 session 恒注册（装配无条件声明 task manager 可用，缺失时回退临时实例）；invoke 只返回“结果会自动注入”的引导文本，`timeout() == None`；子 agent/Workflow Agent 链无 SubAgentMiddleware，不注册 |
| `McpResourceTool` | `mcp_read_resource` / — | key `mcp_read_resource`；Deferred | `McpMiddleware`；MCP pool `has_resources()` 时注册；位于 parent tools，SubAgent 可按 allow/disallow 继承 |
| `McpToolBridge` | `mcp__{sanitized_server}__{sanitized_tool}` / — | 动态 canonical key；Deferred | 每个已连接 MCP tool 运行时生成；非 `[A-Za-z0-9_-]` 字符替换为 `_`；位于 parent tools，SubAgent 可继承 |
| `WriteSandboxTool` | `SandboxWrite` / `WriteSandbox` | child-local key `SandboxWrite`；Deferred | 仅在 SubAgent/Workflow Agent 的 `allowedWriteDirs` 非空时注入本地 registry；不进入主 session shared tools |

**当前基线**：生产非空别名全集是 `reading → Read`、`Shell → Bash`、`task → Agent`、`WriteSandbox → SandboxWrite`，resolver 对 key、canonical name 与 alias 均作 ASCII 大小写无关匹配。旧 `Skill` 只是 TUI 的历史展示兼容名，不是当前注册 alias。定义型 SubAgent/Workflow Agent 的 `tools` allowlist 只按 canonical `tool.name()` 大小写无关匹配，不用 alias 匹配。

**当前基线**：`DirectToolInvocationResolver` 在同一次 registry 扫描中收集 exact key、exact canonical name、大小写无关 key/name 和大小写无关 alias 的候选，并按实例指针去重；零候选为 `ToolNotFound`，多于一个不同实例为 ambiguous failure，不存在“alias 静默覆盖 canonical”的优先级。只有 target schema 声明 `file_path` 时才兼容归一化 `path → file_path`，不得改写 `Glob`/`Grep` 的合法 `path`。

**当前基线**：`ExecuteExtraTool` 是 direct wrapper，真实能力是 resolver 绑定的 target。wrapper 实例自身未覆盖 `timeout()`（声明为默认 120 秒），但生产 dispatch 经 `ExecuteExtraToolResolver` 重写为绑定 target 的 canonical invocation，执行与超时均取 `target.timeout()`；wrapper 的 120 秒在 dispatch 路径不构成截断来源。`AgentResultTool` 是可检索、技术上可 invoke 的 synthetic placeholder，不是后台结果查询 API。`ArtifactTool` 虽由 `ToolSearchMiddleware` 提供，当前 `is_direct()` 明确返回 `true`；源码文件中称其 deferred 的注释是待修正的内部冲突，本文不得沿用该错误分类。

**当前基线**：`BaseTool` 契约的生产覆盖现状（默认值事实源 `peri-acp-types/src/tools.rs`）：

| 契约项 | 默认值 | 生产覆盖 |
| --- | --- | --- |
| `timeout()` | `Some(120s)` | 覆盖为 `None`（不超时）：`Write`、`Grep`、`Bash`、`WebFetch`、`WebSearch`、`AskUserQuestion`、`Agent`、`AgentResult`、`LSP`、`Workflow`、`mcp_read_resource`、动态 `mcp__*`、`SandboxWrite`；`ExecuteExtraTool` 未覆盖（默认 120s，dispatch 路径不生效，见上） |
| `output_char_limit()` | `None` | 仅 `Bash` 为 `Some(10000)`；`WebFetch` 显式返回 `None`；消费点在 `tool_dispatch` 后处理按 call id 查找（见 §3.3 gap #18） |
| `prefers_persist()` | `false` | 仅 `Read` 为 `true`；生产无通用消费点 |
| `context_retention()` | `Preserve` | 无生产覆盖；compact_v2 planner 消费的是 `CompactConfig.tool_retention_map`（生产来自用户配置、默认空），不自动读取 `BaseTool` 声明——声明与消费路径分离 |
| `title()` | `None`（由 `derive_title_from_name` 派生） | 无生产覆盖 |
| `namespace()` | `None` | `filesystem`（Read/Write/Edit/Glob/Grep/folder_operations）、`execution`（Bash）、`web`（WebFetch/WebSearch）、`interaction`（TodoWrite/AskUserQuestion/Agent）、`skills`（SkillTool/DiscoverSkillsTool）、`meta`（SearchExtraTools/ExecuteExtraTool/artifact）；Cron/LSP/Workflow/goal/MCP/SandboxWrite 为 `None` |

#### 4.1.1 Wire、TUI 与目标 presentation 身份

下表把每个工具族的当前投影、目标 renderer 与历史 fallback 分开；“可信”仅指来自 resolver/descriptor snapshot 的字段，不指 TUI 根据 title 猜出的名称。

| 工具或边界 | **当前基线**：live ACP / 当前 TUI | **目标规范**：presentation family | **协议依赖**：可信新增事实 | **兼容回放**：缺现代 metadata 时 |
| --- | --- | --- | --- | --- |
| `Read`, `Glob`, `Grep`, `folder_operations` | `tool_call.title` 是 raw outer name；TUI 以该 title 为 `tool_name`，主要走 Generic | `FilesystemRead/Search/List`；对象是脱敏 path/query，结果是 range/count/excerpt | canonical/effective identity、结构化 range/match/entry counts、completeness | 识别已知 canonical/`reading`；只用已有安全 input/output 生成有界摘要，否则 Generic |
| `Write`, `Edit`, `SandboxWrite` | raw title；`Write`/`Edit` 在通用 renderer 有 diff 局部特判，`SandboxWrite` 无完整专属语义 | `FilesystemMutation`；明确普通 workspace 与 sandbox scope，Preview 为首个 hunk | effective path/input、结构化 effect/diff、sandbox allowed roots、terminal classification | 识别 `WriteSandbox`；无结构化 diff 时只显示中性 mutation 摘要，不从任意文本伪造 patch |
| `Bash` | raw title；通用 card 有 command/duration 局部特判，没有 running stdout/stderr stream | `TerminalExecution` | effective command、exit/background/timeout、stdout/stderr chunks 与 detail ref | 识别 `Shell`；只显示已脱敏 command 与最终摘要，不把普通 output 猜成分流日志 |
| `WebFetch`, `WebSearch` | raw title；Generic | `NetworkFetch/Search` | status、result descriptors、structured counts、detail completeness | 从已知 input 提取脱敏 host/query；未知 output 走 Generic，不展开任意 JSON |
| `artifact` | raw title；Generic；provenance 不在 wire 中 | `PublicArtifact`，始终醒目标记 public upload 与 expiry | canonical identity、public URL/TTL/size 的已脱敏 descriptor | 仍按 `artifact` known family；缺结果 metadata 时显示 `Published artifact · details unavailable`，不猜 URL |
| `TodoWrite` | `tool_semantics` 已解析 input，并按会话内成功快照显示 todo change；只接受当前结构 | `TaskMutation` | 若需跨客户端等价，需持久化 snapshot revision/completeness | 继续解析合法历史 input；无效 input 回 Generic，不能把 output 内部索引 dump 出来 |
| `AskUserQuestion` | tool card 与 AskUser elicitation/interaction block 是两条当前通路，缺稳定 ToolKey 关联 | `DurableInteraction`，决策面而非普通详情卡 | `tool_call_id + request_id + revision + resolved result` 的关联 | 保留可见 pending/resolved block；缺关联时不合并到猜测的 tool card |
| `SkillTool`, `DiscoverSkillsTool` | `SkillTool`/旧 `Skill` 有专属 name presentation；Discover 仍主要 Generic | `SkillLoad/SkillDiscovery` | skill source/descriptor/completeness；正文可见性策略 | 接受旧 `Skill`，但标为 legacy；不默认展示 `SKILL.md` 正文 |
| `Agent`, `AgentResult` | raw `Agent`/alias；`SubagentStarted/Stopped` 另建 `TuiSubAgentGroup`；Agent card 可被 group claim；后台结果经 `MessageKind::Defer` 明文回调在下一轮注入（见 §4.3） | `Delegation/SubagentResult` | parent ToolKey、instance/thread/task identity、mode/model、nested detail ref、终态 | 识别 `task`/大小写变体/历史 `Task`；无法关联的 `AgentResult` 独立标 synthetic，绝不提供“再次查询”动作 |
| `SearchExtraTools` | raw wrapper name/input/output；Generic | `ToolDiscovery` | descriptor snapshot、namespace、direct/deferred、provenance | 从合法 `query` 生成安全摘要；结果无法结构化时只给有界文本 |
| `ExecuteExtraTool` | wire 只见 raw outer wrapper；TUI 可读 input 的 `tool_name`，但不知道是否解析/批准/执行成功 | verified 时委托 effective target family，并尾注 `via ExecuteExtraTool`；未验证时保持 wrapper | requested/canonical/delegated/effective name、wrapper、normalized/effective args、approval provenance | 仅称 `Requested {delegated_requested_name} via ExecuteExtraTool`；不得把 raw `tool_name` 冒充 resolved target |
| `cron_register/list/remove` | raw title；Generic；Cron 管理 Panel 是另一数据面 | `ScheduleMutation/List` | schedule descriptor、next fire、effect、detail/cancel capability | 从 allowlist 字段展示 expression/short ID；缺字段时 Generic，不把 prompt 全文铺开 |
| `LSP` | raw title；Generic | `CodeIntelligence` | operation、locations、diagnostics、server/source、completeness | 只按合法 operation/path/query 展示；普通文本结果不伪造 diagnostics |
| `Workflow` | tool completion 可给 `run_id`；`WorkflowProgress` 事件零构造点（见 gap #25），可见状态只来自每 2 秒 `workflow/list_runs` 轮询 | `WorkflowRun` | ToolKey↔run identity、phase/agent/status/log revisions、detail/cancel capability | 有 run ID 时关联；无身份时保留独立 legacy tool entry，不能按相似名称合并 run |
| `goal` | raw title；Generic | `GoalStateTransition` | action/objective/state/evidence revision | 从合法 action allowlist 生成中性摘要；缺数据时 Generic |
| `mcp_read_resource` | raw title；Generic；原始 MCP server/resource descriptor 不完整 | `ExternalResource` | 原始 server、sanitized URI、content type/size/sensitivity、descriptor snapshot | 只显示可验证的 input 字段；body 视为不可信外部内容并脱敏 |
| 动态 `mcp__*` | raw sanitized callable name；Generic；不能从该名称无损恢复原始 server/tool | `ExternalMcpAction` | 调用时冻结的原始 server/tool/title/namespace/schema/origin 与 effective identity | 以 sanitized callable 作为 legacy label，unknown effect；不推断 read-only/safe，不 dump JSON |
| 未知 future/dynamic tool | raw title；unknown summary 当前可能错误取 object 首字段 | `GenericSafe` | 可选可信 descriptor/presentation family；没有也必须可渲染 | humanize 已清洗 title；主对象只读字段白名单，否则显示参数数量与 unavailable；永不 panic |

**当前基线**：所有标准 live tool call 都从 Agent 的 raw outer call 投影为 ACP `toolCallId + title + rawInput`；TUI 再把 `title` 当 `tool_name`。`ToolEnded` 只提供同 ID 的 `rawOutput/status`；来源轴 `_peri.sourceAgentId` 已注入并解码为 `agent_id`（见 §8.2），但不改变 title/name 语义。因此表中的 target family 不能反向当作已存在的 wire identity。

**目标规范**：Direct/deferred 只决定模型发现路径，不决定视觉层级。目标 UI 只有在拿到 resolver 产生的可信 identity snapshot 时，才能以 effective canonical tool 为主身份、以 wrapper/provider 为次级 provenance。

**兼容回放**：历史 replay 继续保留 raw requested title、input/result 和已知 alias；可从 `ExecuteExtraTool` input 提取 **delegated requested target**，但不得把未验证名称或参数宣称为已解析、已归一化、已批准或已执行的 effective action。

### 4.2 不能静态枚举的边界

**当前基线**：

- MCP 工具名、schema、数量与 server 连接状态运行时变化；
- plugin 不直接注册一个固定“Plugin tool”，但可间接提供 MCP、skills、agents 与 hooks；
- Skills 来自 user/global/project/plugin/builtin 多个根，具体内容由 session catalog 决定；
- `Agent.subagent_type` 来自项目、内置与 plugin agent definitions；
- Workflow script、phase、Agent 集合与输出是动态的；
- 新 deferred 工具默认 `is_direct=false`，应自动落入安全 Generic presenter，而不是空白或 panic。

### 4.3 注册、schema、过滤与有效身份

**当前基线**：`BaseTool` 以 `name()`、`description()`、JSON `parameters()`、`is_direct()`、`aliases()` 以及可选 timeout/output limit 描述能力。`is_direct()` 默认 `false`。主 session 每 turn 把候选工具按 canonical `name()` 注册到共享 map；Reason 阶段只把 `is_direct=true` 的 definition 发送给 LLM；`ToolSearchMiddleware::before_agent` 只把已注册且 `is_direct=false` 的工具建入 deferred index。生产链顺序由 `production_blueprint` 与 `peri-middlewares/src/assembly.rs` 共同约束，遵守 `ARC-MIDDLEWARE-001`；direct/deferred 可见性遵守 `ARC-TOOLS-001`。`BoxToolWrapper`/`ArcToolWrapper` 只委托 `name`/`is_direct`/`description`/`parameters`/`invoke`/`timeout`/`aliases`，不构成新的可见工具名；`output_char_limit`/`prefers_persist`/`context_retention`/`title`/`namespace` 未委托、回落到 `BaseTool` 默认——经 `ArcToolWrapper`（fork 路径）继承的 `Bash` 因此丢失 `output_char_limit=Some(10000)`，namespace/title 也不进入 `tool_description`。

因此以下五个判断轴必须分开记录，任何一项都不能替代另一项：

| 轴 | 含义 | 当前事实 |
| --- | --- | --- |
| Registry | 实例是否存在于本次 dispatch 的工具 map | 由 session/subagent/workflow 装配与 allow/disallow 决定 |
| Direct exposure | schema 是否进入本轮 LLM `tools` 参数 | 仅 `is_direct=true` |
| Deferred discovery | 是否进入当前 Tool Search index | 主 session 中仅已注册且 `is_direct=false`；依赖该 session 安装 Tool Search |
| Executability | raw call 能否被 resolver 唯一绑定并调用 | 可与 direct/deferred 不同；注册但不可见的 target 仍可能被 resolver 命中 |
| Provenance | 哪个 middleware/service 提供实例 | 不决定 direct/deferred；`ArtifactTool` 正是反例 |

**子 Agent 限制**：定义型 SubAgent 可把 `SandboxWrite` 注入本地 registry，但通用 Reason 阶段仍过滤 `is_direct=false`；其子链没有 `ToolSearchMiddleware`，工具表也没有自动加入 `SearchExtraTools`/`ExecuteExtraTool`。因此当前 `SandboxWrite` 是“已注册、resolver 可执行，但不直接暴露且不可 deferred 发现”的 agent-scoped 能力；不能把“注入成功”写成“模型当前可见”。定义型 SubAgent 本地工具表只由过滤后的父工具（Filesystem+Terminal+Web+MCP）加注入的 `SandboxWrite` 构成，`chain.collect_tools` 不再合并，故 `SkillTool`/`DiscoverSkillsTool`/`TodoWrite`/`AskUserQuestion`/Cron/LSP/Workflow/goal/`AgentResult` 均不可见；fork 路径则全量 clone 父工具（含 MCP），无过滤。Workflow Agent 的 `build_tools` 特殊：Filesystem+Terminal+Web 之外显式加入 project-level `SkillTool`/`DiscoverSkillsTool`（project skills 预缓存），再按 agent 边界与 `allowedWriteDirs` 注入 `SandboxWrite`。

**`AgentResult` 限制**：主 session 当前恒将其注册为 deferred 占位，因此可被 `SearchExtraTools` 发现且 resolver 技术上可调用；但 `invoke` 只返回“后台结果会自动注入，请勿主动调用”的引导文本。真正结果不经 synthetic `AgentResult` tool use/result：后台任务完成时 `AsyncRouter` 把 `BackgroundTaskResult` 路由为 `MessageKind::Defer`（source 为 `SubAgentComplete`/`ShellComplete`/`WorkflowComplete`），executor 在下一轮以明文 `human(to_notification())` 注入、Receive 阶段包 `<system-reminder>` 恢复提示。不能把 registry 中的占位 invoke 当作结果查询 API，也不得在 TUI 建模 synthetic tool call 作为后台结果通路。

注册与调用投影必须按下列顺序理解：

1. session/subagent/workflow 装配按配置与运行时资源收集候选 `BaseTool`；
2. allow/disallow、direct/deferred 与 prompt capability gate 决定模型当轮可直接看到或可检索的集合；
3. `SearchExtraTools` 从 deferred index 返回 descriptor；其中动态 `mcp__*` 可检索，但不必预铺在 prompt 的 deferred 清单；
4. `ExecuteExtraTool` 或 direct resolver 对 requested name、canonical name、大小写变体与 aliases 做唯一解析；
5. 仅当 resolved schema 声明 `file_path` 时，参数兼容层才做 `path → file_path`；`Glob`/`Grep` 的合法 `path` 参数不得被改写；
6. middleware/HITL 处理 canonical policy call，并可返回经批准编辑的 input；实际 invoke 绑定 resolved target；
7. **当前事件仍投影 raw outer name/input**，因此 effective identity 与最终 input 只有 Agent 内部可知，属于 §19.2 的协议依赖。

**目标规范**：每条 entry 的 presenter 与安全策略以 verified effective target 为主，以 requested/wrapper/origin 为 provenance；任何过滤、alias resolution、wrapper delegation 或 approval edit 都不得更换 `tool_call_id`。解析失败也必须产生同 ID 的 `NotExecuted` 审计终态，而不是只留到 transcript replay。

**当前基线**：plugin 模块和 plugin ACP 事件提供安装、清单、skills、agents、commands、MCP/LSP/hooks 等扩展面，但没有证据表明任意 plugin 名称会直接自动桥接成独立 `BaseTool`。

**目标规范**：“未知 plugin/future tool”是前向兼容边界：若未来某注册源确实提供动态 `BaseTool`，自动使用安全 Generic presenter；不得把 plugin management event 伪装成 tool call，也不得宣称当前已支持任意 plugin tool 注册。

### 4.4 非 `BaseTool` 的 tool-like activity 契约

这些活动不进入工具 registry，也不应伪造 `BaseTool` identity；但它们会改变用户对“Agent 正在做什么”的判断，必须进入同一 activity workbench。下表逐项区分现状、目标、跨层依赖和旧历史降级。

| Activity / 事件族 | **当前基线** | **目标规范** | **协议依赖** | **兼容回放** |
| --- | --- | --- | --- | --- |
| `ToolCount` | typed event 可达，但 handler 丢弃 count payload，只重推 ACP state | 作为当前 turn 的低权重计数更新；优先并入 Activity strip/相关 Agent summary，不单独制造 transcript 卡 | 若 count 要与 nested Agent/turn 精确关联，需 `turn_id + source_agent_id + revision` | 旧历史无 count 时不补零、不显示未知精确总数 |
| `Progress` | typed event可达，但 handler 丢弃 payload，只重推 ACP state | 只更新拥有稳定 activity identity 的 progress line/Inspector；完成后折叠，不用每个 tick 追加 entry | 需要 activity key、sequence、phase、safe message、optional total；工具级进度可关联 ToolKey | 无稳定 key 的旧 progress 只可作瞬时、脱敏状态；回放不伪造百分比 |
| `usage_update` / `StateSnapshotMeta` | `usage_update` 直接写 `SPINNER_TOKEN_COUNT`；`StateSnapshotMeta` 可写 `CONTEXT_USAGE`，不形成 transcript entry | 保持 status/composer 邻接的 session telemetry；仅阈值跨越形成 System event | 若需可回放预算时间线，需稳定 usage snapshot schema；普通 token tick 不必持久化 | 缺 usage 历史时显示 unavailable，不从文本估算 token |
| `BudgetWarning` / `SystemNotification` | 作为 `TuiSystemNote` 注入 current turn 的时序 segment；level 映射为 info/warning/error | 保持明确 system 来源、级别、安全消息与 recovery；可折叠但错误默认可见 | 需要结构化 code/source/action，不能只依赖本地化前的自由文本 | 旧 note 保留脱敏文本；未知 level 用 neutral/info，不猜严重性 |
| `CompactStarted/Completed/Error` | Started 只进入 loading；部分 applied outcome 注入 SystemNote；多种 failed/shadowed/interrupted outcome 静默；manual completion 通过 pending atom 跨 reset 重建 | 一个稳定 compact activity 原位更新 strategy/trigger/outcome；失败、shadow、commit 后中断都要有可理解终态；不伪装 tool | 需要 compact activity ID、完整 outcome taxonomy、before/after token counts、replay persistence；manual/auto 明确 | 旧历史只有完成 note 时保留 note；缺 started 不伪造 duration；缺 outcome 标 `Legacy compact result` |
| `Prediction` | 写 `PREDICTION` 输入辅助状态；只元数据 action 时保留既有 placeholder | 仅属于 composer assist，不进 transcript/Inspector；采纳或忽略都不得被误记为 Agent tool | 无额外协议即可保持瞬时；若审计采纳结果，应使用独立 composer action 契约 | session replay 不恢复陈旧 prediction |
| `FileSuggestions` | event handler 当前 no-op | 作为 composer 的 anchored suggestion list，服从 popover focus/scroll；不进 transcript | 需实际 suggestions payload、revision 与输入 generation | 旧 session 不回放 suggestion；缺数据时不显示空面板 |
| ACP `plan` update | notifier 直接交给 plan renderer，不进入通用 tool reducer | 作为 durable plan/task surface；与 `TodoWrite`/`goal` 分别显示来源，避免重复计数 | 若需 live/replay 等价，plan item 要有稳定 ID/revision/source | 只有文本/旧 plan 时以 legacy plan 展示，不推断 Todo/Goal identity |
| HITL `RequestPermission` / `HitlPending` | 当前同时创建 inline pending block 和 `HitlPopup`；标准 permission 请求中的 `toolCallId` 在 TUI DTO 转换时丢失；Popup 整窗 Left Down 可直接批准 | 同一 durable interaction 原位进入 AwaitingApproval→resolved；Modal 仅承载决策，body click 永不批准；关联对应 ToolKey | 必须保留 `tool_call_id + request_id + revision + effective action/args + policy scope`，并让等待与 turn cancellation 竞速 | 无 ToolKey 的旧请求保留独立 legacy interaction，按 request ID 去重；不得猜关联或自动批准 |
| `AskUser` / local `InteractionResolved` | AskUser Panel 与 inline block 并存；RPC 成功后 local event 按 request ID 回写；Completed 当前仍默认 Expanded | pending 问题 durable 可见；提交中、失败重试、resolved answer 在同一 entry；完成后默认 Collapsed result | 需要稳定 request revision、question/option IDs、resolved result 与 replay record | 旧 pending 可继续回答的前提是服务端仍确认有效；否则只读标 `Expired legacy request` |
| `OauthNeeded` | 保存 payload 并打开 OAuth Popup；普通 transcript 中没有等价 durable lifecycle | OAuth 是阻塞决策/外部流程 Modal；显示 provider、verification URI 的脱敏 host、expiry 与 waiting/success/error；背景 inert | 需要 OAuth request ID、expiry、completion/cancel event 和安全 URL metadata | 历史 OAuth 不应重新弹出；仅显示已完成/过期 system record，缺结果标 unavailable |
| `RewindPreview/Completed/Error` | `RewindPreview` 已退役/no-op；Completed 以 `messages_json` 重建 user/assistant 文本并会省略 tool/reasoning 等非文本 block；Error 注入 warning note | 候选由 Rewind Panel 按需查询；完成后以 canonical session/load 重建完整 transcript，保留可回放 tool/activity identity；错误提供恢复 | 需要 rewind target/revision、新 session generation 与完整标准 replay；不得以 TUI 自行解析简化 JSON 作为权威历史 | 老服务端 preview 可忽略；简化历史必须标 `Partial rewind replay`，不能让省略的 tools 看起来从未发生 |
| `SubagentStarted/Stopped` + child chunks/tools | 当前创建 `TuiSubAgentGroup`；sync child 内容路由到 child turn；background child 进入 `BG_AGENT_IDS/BG_DISPLAY`；stop 冻结 child trailing 内容 | 父 Agent entry、SubAgent group、后台 activity 与 nested Inspector 使用同一 instance/thread identity；只在主 transcript 留有界摘要 | 需要 parent ToolKey、instance/thread ID、source agent、mode、terminal reason、nested detail revision | 缺 parent link 时保留独立 legacy SubAgent group；不得按相邻位置强行 claim Agent card |
| `BgTaskSnapshot/Started/Completed/Cancelled` | 维护 `BG_TASKS/BG_DISPLAY`；完成/失败有短暂 notification，display entry 约 3 秒后清除 | Activity strip + Background Panel 持久显示 active 与最近 terminal 摘要；transcript 只在用户相关完成/失败时留一条有界记录 | 需要 task revision、kind、parent/continuation identity、terminal reason 与 durable result ref | snapshot 是重连事实源；旧历史无 task record 时不从 Agent 文本猜后台任务 |
| AgentEvent `BackgroundTaskCompleted` | 与上述 `BgTaskCompleted` 不同；该 ACP 事件当前从未被构造（executor 不再经 EventSink 直推，`event_sink` 无映射臂），TUI 对应 handler/notifier 解码臂是不可达死代码 | 与同 task ID 的后台 entry 原位合并，显示 result availability、duration、child thread；不能重复出现两种完成记录 | 若恢复事件面，两条事件必须共享 canonical task ID/revision，或收敛为单一事件源 | 无匹配 task 时创建一条 `Background result received · legacy/unlinked`，不丢失也不重复猜合并 |
| `BgCallbackBubble` / continuation | local handler只先 flush current turn；可见 callback 内容依赖标准 session/update user bubble | 标明 `Background callback`/来源，不能伪装普通用户输入；激活后按普通 turn 继续 | 需要 source task/thread ID、callback kind 与 replay metadata | 旧 callback 仅有 user bubble 时保留文本并标 legacy source unknown；不猜具体 task |
| `WorkflowProgress` | 该事件在 ExecutorEvent/AgentEvent 全树零构造点，TUI handler/notifier 解码臂是死代码；可见状态只来自每 2 秒 `workflow/list_runs` snapshot 轮询 | 事件 reducer 与 snapshot reconciliation 同一 run；Activity strip 显示 phase/status，完整 agents/logs 进 Workflow Panel/Inspector | 需要 `run_id + revision/sequence + phase/agent/run terminal`；明确 snapshot authority 与 gap recovery | 旧历史只有 Workflow tool result/run ID 时保留启动记录；无 journal 不伪造 phases |
| `PluginSnapshot/ActionResult/SearchResult` | 分别更新 Plugin Panel 列表、短通知/刷新、search results；不是 tool call | 保持管理型 Panel/notification，不进入 Tool Inspector；若 plugin 间接提供 MCP/skill/agent，显示该实际来源 | 管理 action 需 request/revision 与安全 error code；不需要伪造 ToolKey | 不回放陈旧搜索结果或重开操作；已安装状态以当前 snapshot 为准 |
| `AgentExecutionFailed` | 注入 Error SystemNote，并把 phase 置 Idle；不逐项 settlement running tools | 保留全局失败 note，同时 settlement 当前 generation 的 open tool/subagent/workflow entries；提供 recovery | 需要 generation、structured failure code、affected activity keys | 旧失败 note 可见；无法确定受影响项时 open entry 标 `Legacy incomplete`，不标成功 |
| `TurnDone/Interrupted/Suspended` | Done flush/reset；Interrupted 有 request ID/本地 generation stale guard和零产出回滚；Suspended deactivate 后归档；均缺精确 per-tool terminal reconciliation | 作为 turn 边界统一 settlement、follow 与 queue 规则；stale 边界不得触碰新 turn | 需要稳定 turn generation/request ID、structured reason、open/finalized activity set | 旧边界按 §8.4 sweep；缺原因时使用 legacy incomplete，不解析英文文本猜终态 |
| `SessionReplayStarted/Done` / replay metadata | typed handlers存在但 notifier 注释为 dead path；实际主要通过标准 `session/update` 的 `_meta.periReplay` 分流 | 明确 replay transaction 边界，结束时统一去重、settlement、anchor/focus 恢复，再原子发布 | 需要 replay ID/generation、start/done 或等价 load transaction signal与 completeness | 无显式边界时以一次 `session/load` transaction 包围；不得逐事件把 viewport反复吸底 |
| transport disconnect | notifier channel close 直接清 loading、输入队列并显示 5 秒断连通知；不经过 activity reducer | 生成 canonical transport system event，settlement 当前 generation，保留可恢复输入策略并阻止旧事件复活 | 需要 transport/session generation 与 reconnect outcome 进入事件链 | 仅本地可知时也必须停止 spinner并标 `Connection lost`；后续 replay 可用 finalized result收敛 |
| `Unknown` / unknown unstable event | notifier 对解码后的 unknown unstable event直接丢弃；typed `Unknown` handler也是 no-op | 保留有界、脱敏的 `Unsupported activity {event_name}` 诊断或计数；默认不打扰 transcript，错误/阻塞事件不能悄悄吞掉 | 至少携带 event name、safe severity、schema version；任意 payload仍不得直送 Inline | 老客户端无法理解时安全忽略 payload，但不得 panic、dump JSON或改变已有 lifecycle |

**目标规范**：非 `BaseTool` activity 与工具 entry 共用稳定 identity、lifecycle、Activity strip、focus/scroll 和安全 presenter 原则，但 surface 服从语义：详情进 Inspector/管理 Panel，阻塞决策进 Modal，composer assist 留在 composer，系统边界留 System event。不得为了“统一”把所有事件画成工具卡。

### 4.5 Agent 上下文装配六路径对照

**当前基线**：以下六条路径是当前源码事实。background 是运行模式而非独立上下文来源：`SubagentRunMode::{Sync, Background}` 是同一 `spawn_subagent` 基础设施内的枚举，frozen/工具/中间件/消息装配相同，仅执行宿主（`tokio::spawn` + TaskManager）、cancel policy 与事件目标不同；`/bg` 命令是同一机制的第二个触发点（`ForkDirectiveKind::Bg`，parent=None）。不得仅凭复用 `spawn_subagent` 或共享 helper 推断各路径上下文完全相同。

| 轴 | Main session | 定义型 SubAgent | fork | background | resume | Workflow Agent |
| --- | --- | --- | --- | --- | --- | --- |
| 创建入口 | `stage_builder` + `production_blueprint` 七组链 | `SubAgentTool::invoke` → `spawn_subagent` | `invoke_fork` → 同入口 | 同入口，`run_mode=Background` | 同入口，从已持久化 child thread 恢复 | `AgentExecutor` 回调 → `build_v2_subagent_context` + `run_react_loop` |
| frozen CLAUDE.md/skills/date | session/new 冻结并存于 `SubagentHost`（主 v2 `Session::store().frozen` 为空） | 从父 store copy（一级子 agent 因父 frozen 为空得到 `Some("")`，遮蔽 host 回退） | 同左；system prompt 取 host 冻结值 | 同定义型/fork | 同父 copy，不重读磁盘；`skill_names` 恒空 | 从 `FrozenSessionData` 注入 `WorkflowAgentContext`；system prompt 优先冻结值 |
| transcript/messages | 绑定主 thread | 全新 transcript + identity System + prompt；不注入 parent_messages | 注入 `parent_messages` + `build_fork_directive` | 同定义型/fork，运行于 `tokio::spawn` | 重放 `ancestor`；prompt 缺省 `IMPLICIT_CONTINUE_PROMPT` | 内部自建 session；无 parent messages |
| 工具表 | 蓝本链 `collect_tools` 合并 shared_tools | 过滤后父工具（Filesystem+Terminal+Web+MCP）+ `SandboxWrite`；本地 map 独立构建，不再合并 collect_tools | 父工具全量 clone，无过滤 | 同 fork | fork 分支 clone；定义型分支按 `meta.title` 重过滤（权限漂移防护） | `build_tools`（Filesystem+Terminal+Web+project `SkillTool`/`DiscoverSkillsTool`）+ agent 边界 + `allowedWriteDirs`→`SandboxWrite`；无 ToolSearch、无嵌套 Agent 工具 |
| middleware 链 | AgentsMd→AgentDefine→Plugin→Skills→SkillPreload→AtMention→Image→Filesystem→GitAttribution→Terminal→Web→Todo→Cron→Hook→Hitl→SubAgent→Mcp→Workflow→ToolSearch→Lsp→Goal | AgentsMd→Skills→[SkillPreload]→Todo（无 ToolSearch/HITL/Cron/Plugin/AtMention/AgentDefine/GitAttribution） | 同左 | 同左 | 同左 | AgentsMd→Skills→SkillPreload→Filesystem→GitAttribution→Terminal→Web→Todo→HITL（条件） |
| persistence/thread | session/new 建 thread，transcript 绑定 | 子 thread：`parent_thread_id` 挂链、`hidden`、`thread_id = agent_id`、`cancel_policy`、`snapshot_at_message_id` | 同左 | 同左 | 复用同一 thread（校验存在/非 active/父链；`RESUME_LOCK` 防双恢复） | 无 child thread、无持久化 |
| sandbox | 不注入 | `allowedWriteDirs` 非空且未被 disallowed 时注入 | 不注入 | 同 fork | 定义型分支重过滤时再注入 | 按 `allowedWriteDirs` + 边界检查注入 |
| `AgentResult` 注册 | 恒注册（deferred 占位） | 不注册 | 不注册 | 不注册 | 不注册 | 不注册 |
| 后台完成通知 | `on_bg_complete` → `AsyncRouter` 入 inbox（`MessageKind::Defer`）→ `ContinuationRequest` → 下轮 `<system-reminder>` 明文注入 | 作为 bg 子 agent：`bg_event_sender` + `on_bg_complete` 回调 + thread 状态收尾 | 同左 | 同左 | 同左 | 不产生 bg 任务 |
| 事件转发 | `spawn_eventbus_forwarder`（v2→ExecutorEvent） | `spawn_subagent_event_forwarder`：`source_agent_id = child_thread_id`；Start/Stop 不转发（发射侧已 v2+v1 直发，防双发） | 同左 | 同左（指向 `bg_event_sender`） | 复用 run_sync/background 路径 | `forwarder_launcher` + `publish_hook`（bridge=None） |
| 取消语义 | cancel token + `cancel_cascade` + session 级 `cancel_all_agents` | `Cascade`（父 token → 子 token） | `Cascade` | `Independent`（新 token，仅 session 级可停；`BgCancelHandle::Abort`） | 依持久化 `meta.cancel_policy`；Cascade 时从父重派生 token | `ctx.cancel` 或内部新建 token |

**目标规范**：TUI 不得按“创建入口”猜嵌套活动语义；统一按 instance/thread/task identity 展示 parent/child/background/resumed 关系，来源路径只作为 provenance 显示。背景任务完成、SubAgent 结果与普通工具结果必须保持三个独立 surface，不得因外观相似合并。

**协议依赖**：若需在 wire 上区分 fork/background/resume 与普通 SubAgent 的嵌套关系，需要 parent ToolKey + instance/thread ID + mode 字段；当前 `_peri.sourceAgentId` 只提供 `child_thread_id` 级来源标识。

**兼容回放**：历史 session 缺少 thread/task 元数据时，只按 replay 中可见的 `SubagentStarted/Stopped`、`BgTask*` 与 tool call 保留独立 legacy 分组，不伪造 parent-child 关系或 mode；Workflow Agent 无持久化 thread，其历史只能靠 Workflow tool result/run ID 还原。

## 5. 方案选择：Inline-first Workbench

**目标规范**：本 redesign 采用 Inline-first Workbench；下述被舍弃方案不是当前实现能力声明。

本设计比较过三种方向：

1. **无限内联展开**：实现简单、上下文连续，但长日志/diff 会破坏 transcript、滚动与性能；
2. **常驻 Inspector 优先**：长内容清晰，但窄屏差，普通成功调用也会占据大量布局；
3. **桌面式鼠标工作台**：宽屏信息密度高，但多窗格、拖动布局和 hover 依赖不适合 SSH/tmux 与键盘用户。

最终采用综合方案：

- 主路径使用紧凑 Inline activity row；
- Expanded block 只做有界快速核对；
- 完整内容进入单实例、非模态、响应式 Tool Inspector；
- Modal 只承担阻塞决策；
- 鼠标使用统一 semantic HitMap 与 command router；
- 不引入永久三栏、可拖动 pane 或卡片内嵌 scrollbar。

## 6. 页面结构与视觉语言

**目标规范**：本节所有布局、视觉 token、状态符号与 tool row anatomy 均是 redesign 完成后的要求；延续当前 `GridSpec` 的位置会逐项明确说明。

### 6.1 基础布局

```text
Wide（Inspector 打开）
┌──────────────────── transcript ───────────────────┬─ Tool Inspector ─────┐
│  user / reasoning / tool / assistant              │  Overview Input ...  │
│  同一条连续时间轴                                 │  独立滚动            │
├───────────────────────────────────────────────────┴──────────────────────┤
│ transient status · composer · key hints                                  │
└───────────────────────────────────────────────────────────────────────────┘

Standard / Compact
┌───────────────────────────────────────────────────────────────────────────┐
│ transcript                                                               │
├──────────────── Tool Inspector bottom drawer ─────────────────────────────┤
│ transient status · composer · key hints                                  │
└───────────────────────────────────────────────────────────────────────────┘
```

- transcript 继续使用 `outer(1) + accent(1) + gap + content + scrollbar(1)` 网格；
- Wide/Standard/Compact/Narrow 断点继续以 `GridSpec` 为事实源；
- Inspector 的 placement 使用额外可用空间判断，不改变 transcript 的既有断点语义；
- composer 在非模态 Inspector 打开时必须保持可见和可操作；仅 Narrow 全屏详情页可暂时隐藏 composer；
- Modal 位于所有 workspace surface 之上，背景 inert。

### 6.2 Calm Workbench 风格

新风格不是“更多边框”，而是更清晰的状态、对象和可操作性：

- 主体保持 `surface.base`，expanded preview 使用低对比 `surface.raised`；
- code/log 使用 `surface.sunken`；
- Inspector 只绘制一条分隔边和固定 header，不给每个 section 套框；
- hover 只强调可操作的 header、disclosure 或 action，不涂满整个 entry；
- focus 比 hover 更明显，使用 outer focus rail + modifier；
- pressed 只作用于被按下的 semantic target；
- 所有 hover/focus action 使用预留列，出现时不得推动 summary 或 duration。

### 6.3 语义 token

延续 Tokyo Night 默认方向，但组件只能请求语义角色：

| Token | 默认方向 | 用途 |
| --- | --- | --- |
| `surface.base` | `#24283B` | transcript 主背景 |
| `surface.raised` | `#292E42` | preview、Inspector section |
| `surface.sunken` | `#1A1B26` | code、diff、terminal output |
| `surface.hover` | 由 theme 从 raised/selection 派生 | 可点击 header/action hover |
| `surface.selection` | `#283457` | keyboard focus、文本选择 |
| `text.primary` | `#C0CAF5` | 正文、主要对象 |
| `text.secondary` | `#A9B1D6` | 结果摘要、section body |
| `text.muted` | `#737AA2` | metadata、provenance |
| `text.dim` | `#565F89` | duration、折叠提示、边线 |
| `accent.user` | `#7AA2F7` | user entry |
| `accent.assistant` | `#BB9AF7` | assistant entry |
| `accent.reasoning` | `#545C7E` | reasoning entry |
| `accent.tool` | `#737AA2` | 普通完成工具 |
| `status.running` | `#7DCFFF` | 运行中 |
| `status.success` | `#9ECE6A` | 成功 |
| `status.warning` | `#E0AF68` | 待审批、部分完成、截断 |
| `status.error` | `#F7768E` | 失败、超时 |
| `focus.strong` | 由当前 palette selection 派生 | focus rail、选中 action |
| `syntax.command` | `#E0AF68` | command |
| `syntax.path` | `#FF9E64` | path / location |

Hex 仅是默认主题方向，不得写入具体组件。

### 6.4 状态符号与文本后备

| 生命周期 | Symbol | 必备状态词 | ASCII fallback |
| --- | --- | --- | --- |
| Queued | `·` | `Queued` | `.` |
| Resolving | `…` | `Resolving` | `~` |
| AwaitingApproval | `!` | `Needs approval` | `!` |
| Running | `◐` | `Running` | `*` |
| Succeeded | `✓` | `Done` | `+` |
| Failed | `×` | `Failed` | `F` |
| NotExecuted | `◇` | `Not executed` | `N` |
| Denied | `⊘` | `Denied` | `D` |
| Cancelled | `■` | `Cancelled` | `C` |
| Interrupted | `‖` | `Interrupted` | `I` |
| TimedOut | `◷` | `Timed out` | `T` |

Collapsed/Expanded 使用 `▸`/`▾`，ASCII 为 `>`/`v`。颜色关闭、Unicode 能力不足或 reduced-motion 时，状态词与 focus marker 仍必须存在。

### 6.5 Tool row anatomy

```text
outer accent  verb      primary object                  result · duration  action
  ›     ✓     Read      src/lib.rs                      184 lines · 37ms   Details
```

规则：

- `verb` 是面向人的稳定动作词，原始 tool name 只作为 fallback；当 canonical name 本身就是稳定动作词（例如 `Read`）时可直接复用；
- `primary object` 回答“对什么做”；路径优先 project-relative，URL 只显示脱敏后的 host/path；
- `result` 回答“结果怎样”，不得重复 primary object；
- duration 在宽度足够时右对齐，Compact/Narrow 优先隐藏；
- action 只在 hover/focus 时增强，但预留宽度或覆盖 metadata 空位，禁止布局跳动；
- Inline 禁止 raw JSON、完整 stdout、文件正文、prompt、skill body 与 workflow script；
- 无安全摘要时显示 `Friendly Tool Name · N parameters`，不得猜测任意字段值。

## 7. 四级信息披露

**目标规范**：每个可审计 activity 使用以下渐进披露层级；数据缺失时显示 completeness/unavailable，不提升为伪造详情。

### 7.1 Inline summary：永久审计记录

每次调用始终保留一条 Inline entry：

```text
{status} {verb} {primary object} · {result/risk metadata} · {duration}
```

- success 默认单行折叠；
- running 显示 elapsed 与至多一条短 activity；
- error、denied、cancelled、timed out 保留明确安全摘要；
- 同一调用的后续事件原地更新；
- Inline 始终可被 keyboard focus、semantic copy 与 Inspector 定位；
- 相邻低信息成功项可以分组，但组内每个 `ToolKey` 仍可访问。

### 7.2 Preview / Expanded：有界原位核对

`Collapsed`、`Preview`、`Expanded` 继续作为 transcript presentation state，但 **Expanded 不再等于完整无限输出**：

- Preview：running tail、结果采样或一段错误摘要；
- Expanded：首个 diff hunk、首批搜索结果、结构化 key-value 摘要或更多错误上下文；
- Expanded 与 transcript 共用父滚动，不得创建内部 scrollbar；
- Wide/Standard 默认最多约 8 个视觉行，Compact 最多 4 行，Narrow 最多 3 行；具体预算由 viewport 与 theme density 决定；
- 超出时显示 `… N more lines · Open details`；上游未知省略量时显示 `More content available`；
- success 可自动折叠；running 默认 Preview；error 默认 Expanded summary；
- 用户手动改变 fold 后，lifecycle 更新不得覆盖 `user_modified`。

### 7.3 Tool Inspector：完整非模态详情

Inspector 用于完整或协议可达范围内的：

- input/output、stdout/stderr、diff、搜索结果与文件 excerpt；
- LSP diagnostics/locations；
- MCP resource/structured result；
- SubAgent nested transcript；
- Workflow phases、agents、logs 与 result；
- wrapper/effective tool、origin、approval 与 truncation metadata。

它不承担批准、拒绝、提交问题或修改权限模式。详见 §10。

### 7.4 Modal：仅阻塞决策

Modal 只用于：

- HITL permission 与高风险批量审批；
- 不可逆动作确认；
- OAuth；
- 多题、自由文本或复杂 AskUserQuestion 表单；
- 其他必须先响应才能继续的安全决策。

普通 tool detail、长日志、diff 和 nested transcript 不得进入 Modal。详见 §11。

## 8. 生命周期、身份与 UI 状态

### 8.1 三个正交状态维度

```text
Lifecycle（目标）:
Queued → Resolving → AwaitingApproval → Running
           └────────→ NotExecuted
                         AwaitingApproval → Denied | Cancelled | TimedOut
                                  Running → Succeeded | Failed | Cancelled | Interrupted | TimedOut

Presentation:
Collapsed | Preview | Expanded

Detail:
Closed | Open { surface, facet, scroll, follow }
```

**当前基线**：live tool entry 通常从审批完成后的 `ToolStarted` 才出现；拒绝会补发 start/end，但 resolver/malformed/middleware identity failure 没有 live entry。ACP/TUI 终态主要只有 completed/failed，TUI `EntryStatus` 只有 Running/Completed/Error。`TurnInterrupted`/`TurnSuspended` 可让部分未结束 entry 停止 spinner，却会退化成 Completed；`TurnDone` 未收到 `ToolEnded` 时还可能保持 Running。

**目标规范**：

- `Queued/Resolving` 表示调用已被 Agent 接收但尚未绑定可执行 target；解析或调用身份校验失败进入 `NotExecuted`，不得冒充执行失败；
- `AwaitingApproval` 必须在同一 ToolKey 上原位更新；`Denied` 表示用户或 policy 明确拒绝，`Cancelled` 表示执行前撤销或明确取消，`Interrupted` 表示已开始活动被 turn/session/transport 终止；
- `TimedOut` 必须带 `phase: Approval | Execution | Detail` 与安全 duration，不能只靠错误字符串识别；
- terminal payload 必须携带结构化 `status + reason_code + safe_message`；`is_error` 只作旧客户端兼容投影；
- lifecycle 终态单调且 exactly-once，迟到事件不得将终态改回 Running；
- lifecycle、presentation 与 detail 互不覆盖；打开 Inspector 不改变 fold；
- `NotExecuted`、`Denied`、`Cancelled`、`Interrupted`、`TimedOut` 不得折叠成普通 `Failed`；
- 空错误使用本地化安全 fallback（语义为 `Tool failed; no details were provided`），不得渲染空块；
- turn 结束但缺少 terminal tool update 时，entry 必须离开 spinner，并按结构化 turn reason 标为 Cancelled/Interrupted；无法判定的兼容数据标为 `Legacy incomplete`，不得显示 Done；
- pending interaction 不参与成功分组。

### 8.2 稳定身份

目标身份模型：

```text
EntryKey {
  session_id,
  kind,
  stable_id,
}

ToolKey {
  session_id,
  turn_id_or_generation,
  source_agent_id?,
  tool_call_id_or_audit_id,
}
```

**当前基线**：当前事件契约以 `RenderEvent::{ToolStarted, ToolEnded}.tool_call_id` 配对；进入 `peri-tui` 的 `TuiToolStarted`/`TuiToolEnded` 与 `TuiToolCard` 后字段名为 `tool_id`。来源轴已部分落地：`event_sink` 对所有 mapped tool 事件无条件注入 `_peri.sourceAgentId`，notifier 解码为 `agent_id` 存入 `TuiToolStarted/TuiToolEnded` 并用于 SubAgent/BG 路由；副作用是主 agent 自身工具事件也带 `agent_id`，TUI 恒走 “subagent tool start NOT ROUTED” 告警并兜底 `start_tool`。`AcpEventWithEpoch` 这个类型名并不代表真正的 epoch：它当前只携带 `active_session_id`；`BridgeState.turn_generation` 只用于 user submission / stale `TurnInterrupted` 防护，turn generation 没有附在 tool event 或 tool card 上。live `end_tool` 因而只按当前容器中的 `tool_id` 匹配；历史 committed 卡也只按 ID 与 running 状态匹配。

**目标规范**：本文统一把 wire ID 或历史 synthetic audit ID 包装为 `ToolKey`；新增协议不得再引入语义重叠的第三套 ID。至少用 `(session_id, turn_id_or_generation, source_agent_id?, tool_call_id_or_audit_id)` 隔离跨 turn、父/子 Agent 和迟到事件：

- focus、fold、hover、press、Inspector selection 和 replay 合并均使用稳定 key；
- vector slot、content hash 与可变行号只能做缓存/定位辅助，不能成为唯一业务身份；
- resize、rewind、session switch 或 generation 不匹配时，旧 hit region 和 pending gesture 必须失效；
- approval RPC 的 `request_id` 只标识一次交互请求，不得替代 ToolKey；单项与 batch item 都必须显式携带对应 `tool_call_id`，同一个 ToolKey 可记录多个 interaction revision。

**协议依赖**：Agent/ACP 的 tool start、approval、output、terminal 与 turn-terminal event 必须携带同一稳定 `turn_id/generation` 和 `source_agent_id`；TUI 本地 submission counter 不能作为远端事件所属 turn 的可信替代。resolver 失败或 malformed call 仍须获得 audit ID，以便产生 `NotExecuted`。

**兼容回放**：历史事件有非空 ID 时，以 replay 顺序和 enclosing message/turn ordinal 构造稳定 legacy key，不能只用裸 `tool_id`；缺少可靠 ID 时，在 session 内按持久 replay ordinal 生成 synthetic audit ID 并标为 legacy。同一份历史每次 load 必须得到相同 key；不得把两次空 ID、跨消息复用 ID 或父/子 Agent 同 ID 合并。

### 8.3 目标 ViewModel

**目标规范**：

```text
ToolEntryVm {
  key,
  identity,
  family,
  lifecycle,
  summary,
  preview,
  detail_ref,
  sensitivity,
  capabilities,
  duration,
  approval,
}

ToolIdentity {
  requested_name,
  canonical_name,
  delegated_requested_name?,
  effective_name,
  wrapper_name?,
  title,
  namespace?,
  origin?,
}

DetailRef =
  Inline { completeness }
  | Remote { opaque_ref, revision, completeness }
  | Unavailable { reason }
```

身份字段语义不得混用：

- `requested_name`：模型发出的 raw outer name；alias、大小写变体和 `ExecuteExtraTool` 都按原值保留；
- `canonical_name`：outer name 经唯一、大小写无关 alias resolution 后的 canonical tool name；
- `delegated_requested_name`：wrapper input 声明的目标名称，只表示请求意图，尚不等于成功解析的 target；
- `effective_name`：resolver 最终绑定并实际执行的 `target.name()`；普通调用通常等于 `canonical_name`；
- `wrapper_name`：存在委托包装时的 canonical wrapper，例如 `ExecuteExtraTool`。

**当前基线**：`CanonicalToolInvocation` 已在 Agent 内部持有 raw call、canonical `policy_call`、`target` 与 `wrapper_name`，但 `ToolStarted/ToolEnded` wire projection 只发送 raw outer name/input。

**协议依赖**：`canonical_name`、`effective_name`、normalized/effective input、wrapper 和 approval provenance 要成为可信 UI 事实，必须按 §19.2 扩展完整事件链。

**兼容回放**：仅从 raw wrapper input 推导的值必须标为 `delegated_requested_name`；历史 replay 只保留提交 transcript 中的 raw tool use/result 时，同样走 legacy 投影，不填造其余字段。

`capabilities` 只描述 UI 可执行的安全命令，例如 `can_expand`、`can_open_detail`、`can_copy_summary`、`can_cancel`；它不得推断后端权限策略。

### 8.4 Live、异常 settlement 与 replay 等价

**当前基线**：正常路径及异常 reducer 行为如下。这里的“no-op”是代码事实，不代表目标接受静默丢失。

| 输入序列/边界 | **当前基线**：live reducer | **当前基线**：replay reducer | 当前可观察问题 |
| --- | --- | --- | --- |
| start → end | `start_tool` 建卡，`end_tool` 更新第一张同 ID 且 output 未设置的卡；duration 在 end 冻结 | replay start 直接 push running card，end 更新 committed 中第一张同 ID running card | happy path 可见，但两条 reducer 不是同一归约实现 |
| duplicate start | 同 turn 同 ID 不新增；仅当新 input 非 null 且旧 input 为 null 时升级 input/presentation | 每次 start 无条件新增 running card | live/replay 卡片数量可能不同 |
| duplicate end | 第一条完成后，后续找不到未完成卡，no-op | 第一条更新后，后续通常找不到 running 卡，no-op | 没有 anomaly 记录 |
| end-before-start | 找不到卡，静默 no-op | 找不到 running committed 卡，静默 no-op | 调用完全不可审计 |
| 空/缺失 ID | Agent 正常生产 dispatch 阻止空/批内重复 ID 发事件；异常 wire 到 TUI 后空串仍可建卡并与其他空 ID 去重 | 空串可成为多张 replay 卡的共同 ID | malformed 历史可能互相覆盖 |
| 同 turn ID reuse | 第二次 start 被当作 duplicate；第二次调用无法独立存在 | 第二次 start 新增；end 按第一张 running 匹配 | 两条路径均不能可靠表达两个真实调用 |
| 跨 turn ID reuse | `CurrentTurn` reset 后可再建卡，但事件本身无可信 turn generation | committed 中跨消息同 ID 可并存，end 只找第一张 running | late end 可能丢失或错误结算，无法凭裸 ID证明归属 |
| TurnDone 前缺 end | 未先 deactivate 即 flush，卡可能永久保持 Running | replay 若缺 result，同样可留下 running card | 终止后仍 spinner |
| TurnInterrupted / TurnSuspended 前缺 end | 先 deactivate 再构建 VM，未结束工具退化为 Completed；没有 interrupted/suspended 状态 | 历史只有边界而无结构化 tool terminal 时无法还原 | 误报成功 |
| late end（turn reset 后） | 当前容器已无卡，静默 no-op | 找不到目标 running 卡时 no-op | 丢失真实终态；无 late/anomaly 指示 |
| transport disconnect | notifier 直接清 `is_loading`/输入队列并显示短通知，不 settlement bridge 中 running tools | load 后只以持久历史为准 | 屏幕可留下 Running，且与重放不同 |
| session clear/switch/reset | 按 `active_session_id` 过滤旧 session，reset 清 committed/current turn/cache 状态 | 新 session 重建 | 有 session 隔离，但没有 per-tool turn epoch |
| `TurnCommitted` / finalized snapshot | Agent 生成快照；生产 EventSink 丢弃对应事件；TUI handler 即使被调用也只刷新、不消费 snapshot | 无 reconciliation | 无法补偿丢失 ToolEnded 或 live/replay output 分叉 |

**当前基线**：live 与持久结果还存在 finalized output 时序差异：

```text
live:
invoke result
  → ToolEnded(raw output, is_error)
  → run_after_tool
  → error_suggest
  → output_char_limit
  → transcript commit

session/load replay:
committed transcript
  → standard session/update tool_call / tool_call_update
  → replay metadata
  → TUI replay reducer
```

因此 live entry 看到的是后处理前 output，而 replay entry 看到后处理后的 transcript output；resolution failure 还可能只在 replay 出现。`RenderEvent::TurnCompleted.finalized_messages` 当前也没有形成 reconciliation。

#### 8.4.1 目标归约与 settlement 矩阵

| 场景 | **目标规范**：确定性 reducer / 最终 UI | **协议依赖** | **兼容回放** |
| --- | --- | --- | --- |
| start → end | 同一 ToolKey 原位从预执行/Running 单调进入精确 terminal；只保留一个 entry | start/end 携带稳定 ToolKey、sequence 与结构化 terminal | 用 legacy key 顺序配对；保留 raw evidence 来源 |
| duplicate start | 同 sequence 完全幂等；较新 revision 只升级允许变动的 input/descriptor，不重置 started time 或 terminal | `sequence/revision` 与 immutable identity | 相同 replay record 去重；无法证明相同时保留独立 legacy entry并标 anomaly，不静默覆盖 |
| duplicate end | 相同 terminal revision 幂等；冲突 terminal 不覆盖 first finalized result，记录 `Conflicting terminal update` 于 Metadata/诊断 | terminal revision/fingerprint | 选用可证明与 start 配对的第一个结果；冲突证据保持可审计，不渲染重复正文 |
| end-before-start | 创建 synthetic/orphan entry，直接落 terminal 或 `Legacy incomplete`，显示 `Start event missing` | terminal event 自带完整 identity/安全摘要 | 以 result ordinal 建 synthetic key；绝不丢弃，也不伪造 started time/input |
| 空/缺失 ID | 每个 record 获得独立 audit ID；显示 `Invalid tool identity · not executed/legacy` | Agent 为 malformed call 分配 audit ID 并发 `NotExecuted` | 按 enclosing message + ordinal 生成稳定 synthetic ID；多个空 ID 不合并 |
| ID reuse | `(session, turn/generation, source_agent, call_id)` 分离；同一完整 key 内的 reuse 才算冲突 | wire 传可信 turn/generation/source agent | 用 replay message/turn ordinal 扩展裸 ID；同 ID 多卡按顺序一一配对 |
| late event | generation 不匹配的事件不能修改新 entry；可关联旧 entry 时只允许补齐非冲突终态，并标 late；否则进入 orphan audit | event 带 turn/generation 与 sequence | 不根据“当前第一张 running 卡”猜配对；证据不足则 synthetic orphan |
| TurnDone | 所有 open tools 必须 settlement；正常结束却缺 tool terminal 时标 `Legacy incomplete`/`Orphaned`，绝不继续 spinner或显示 Done | turn terminal 提供 generation、reason、open/finalized ToolKeys 或权威 finalized snapshot | replay 结束时 sweep 仍 running 的 legacy entry，标 `Legacy incomplete` |
| TurnInterrupted | 已开始 → `Interrupted`；尚未执行 → `Cancelled`；保留安全 reason | 结构化 turn reason、tool execution phase | 缺 phase 时使用 `Interrupted · legacy reason unavailable`，不得 Completed |
| TurnSuspended | 真正后台移交的调用标 `Suspended/Background` 并关联 continuation；其余 open 调用不得被视为成功 | continuation/task identity 与 open ToolKey set | 无 continuation 证据时标 `Legacy incomplete`，不猜 background |
| transport disconnect | 立即停止所有当前 transport generation 的 spinner；已开始标 `Interrupted · connection lost`，待审批标 `Cancelled/Unavailable`；重连不得复活旧 generation | transport lifecycle 进入 canonical event chain并关联 session generation | 重放若有后来持久 terminal，以 finalized record 替换临时 disconnect settlement；否则保留不完整标记 |
| replay 缺 end | load 完成 sweep 为 `Legacy incomplete`；Inspector 明确 `Terminal result was not recorded` | 可选 replay boundary / completeness metadata | 必须实现，不依赖新协议；不永久 running |
| replay duplicate/冲突 | 使用稳定 ordinal、record fingerprint 与一一配对规则收敛；Metadata 暴露 anomaly count | 新历史可持久化 stable key/revision | 老历史保守保留证据，不把两次调用折成一次成功 |

**目标规范**：所有 reducer 都必须满足 terminal 单调、duplicate 幂等、冲突可审计、未知事件不 panic。settlement 只结束 lifecycle，不得清除 entry、手动 fold、Inspector selection 或已加载 detail。

**协议依赖**：定义唯一 `FinalizedToolResult`，在 `after_tool`、`error_suggest`、截断、持久化 metadata 与 terminal classification 完成后生成；live terminal update 与 transcript 必须由这一个值投影，禁止各自重算。如仍需低延迟原始 completion，可单独发非终态 `ExecutionFinished`，但不得让它冒充 finalized terminal result。

**协议依赖**：`session/load` 必须继续在返回前，以标准 ACP `session/update` 重放 user/assistant/thought 与 `tool_call/tool_call_update`；扩展 metadata 只补充 stable identity、revision、terminal reason、completeness 和 anomaly evidence，不创建另一套视觉模型。turn terminal/transport event 必须 reconciliation 当前 generation 的 open ToolKeys。

**兼容回放**：live 与 replay 对相同逻辑调用最终应归约为等价的 identity、lifecycle、safe summary、completeness 与 durable interaction result；“等价”不要求历史伪造新 metadata。旧 session 不得伪造 effective identity、approval provenance、精确 duration 或完整详情；证据缺失必须以 `Legacy`/`Unavailable` 可见表达。

### 8.5 Presenter 管线

**目标规范**：

```text
resolver identity snapshot + raw tool event / legacy replay
  → project requested/canonical/delegated/effective identity
  → descriptor snapshot / family classification
  → source-side redaction + terminal-control sanitization
  → semantic summary / preview / detail facets
  → ToolEntryVm
  → shared renderer
```

选择顺序：

1. 有可信 resolver snapshot 时，按 effective exact tool；
2. 否则按 canonical exact tool 或已知 alias；
3. 有可信 descriptor 时，按 namespace/prefix/family；
4. safe Generic fallback。

`ExecuteExtraTool` 的 legacy event 只有 raw wrapper input 时，可显示已脱敏的 `requested {delegated_requested_name} via ExecuteExtraTool`，但不得据此声称 canonical target、effective args、审批后 input 或 target risk 已知。Renderer 只消费已脱敏的语义模型；不得在 render body 临时解析 raw JSON、推断风险或写业务 atom。

## 9. 详情数据通路与安全边界

### 9.1 当前可达性

**当前基线**：Agent/ACP 事件携带 `ToolStarted.input` 与 `ToolEnded.output`，ACP 映射为 `rawInput/rawOutput`；但 TUI 在 notifier/ViewModel 边界把结束事件压缩为字符串摘要，且没有运行中 output chunk。因此：

- completed output 可在只改造 TUI 解码的前提下更完整地接收；
- JSON object/array 的 wire 数据已存在，TUI 必须保真处理，不能再变为空串；
- **协议依赖**：实时 Bash tail、stdout/stderr 分流、结构化 full diff、分页历史 detail；
- **目标规范**：没有结构化数据时不得从普通文本伪造 stdout/stderr 或完整 diff。

### 9.2 推荐：混合 detail 通路

**目标规范**：采用“小型脱敏详情随事件 + 大型内容按需读取”的混合方式；其中 summary/preview 的 TUI 表示属于目标 UI。**协议依赖**：opaque ref、revision、分页 RPC 与 source-side redaction 属于跨层协议工作：

- audit summary、状态、descriptor、preview 与 detail availability 始终随事件/replay 到达；
- 小型、已脱敏的 input/output 可 inline；
- 大 output、diff、log、resource、nested transcript 只发送有界 preview、revision 与 opaque ref；
- Inspector 打开或已打开 facet revision 变化时，经 ACP transport 按 `ToolKey`/opaque ref 分页读取；
- hover 不得触发详情请求；
- 请求携带 session 与 generation，快速切换 tool/session 后丢弃迟到响应；
- **兼容回放**：旧 session 无 detail ref 时显示 `Full detail was not recorded for this session`，并保留已有安全摘要。

详情请求只能接受 opaque handle 或 tool identity，不接受客户端任意文件路径，防止把 Inspector 变成绕过工具权限的读取接口。

### 9.3 Redaction 与终端安全

**协议依赖**：安全边界必须位于 wire/持久 ViewModel 之前。**目标规范**：TUI 可做 defense-in-depth，但不得成为唯一脱敏层。

至少覆盖：

- key：`token`、`password`、`secret`、`authorization`、`cookie`、`api_key`、private key、connection string；
- URL userinfo 与敏感 query value；
- HTTP headers；
- Bash 环境变量赋值和常见 secret flag；
- `.env`、credentials、private-key 等敏感路径的内容；
- MCP/plugin 未知 JSON 中的敏感 key；
- ANSI、OSC、控制字符、超深 JSON 与超长单行。

约束：

- Inline、Expanded、Inspector、Modal、copy、debug ViewModel、错误和 telemetry 使用同一脱敏结果；
- 不提供 `Reveal secret`；
- semantic copy 只能复制已脱敏内容；
- unknown tool 不读取任意首个字段；
- Skill/system prompt、Workflow script 等内部指令默认不进入 Inline，详情仅在来源可见性策略允许时展示；
- truncation 必须明确区分 `Complete`、`Truncated` 与 `Unavailable`，不得以视觉上“像完整”误导用户。

## 10. Tool Inspector

**目标规范**：Inspector 是非模态详情 surface；本节 placement、facet、加载与 Panel/Popup 边界均为目标行为，不表示当前 `PanelKind` 已具备这些能力。

### 10.1 形态与 placement

Inspector 是单实例、非模态、session-scoped 的详情 surface。打开新 tool 时替换当前 selection；可通过前后导航在同一 turn 的 tools 间移动。

| 可用空间 | 形态 | 约束 |
| --- | --- | --- |
| 宽度 `>= 120` 且 transcript 可保留至少 60 cells | 右侧 dock | 宽度约占 35%–40%，建议钳制在 42–72 cells；transcript 与 Inspector 独立滚动 |
| 宽度 60–119 且高度充足 | composer 上方 bottom drawer | 不隐藏 composer；transcript 至少保留 3 行；drawer 不超过 workspace 可用高度的一半 |
| 宽度 `< 60` 或高度 `< 12` | 全屏 detail page | 单一详情滚动上下文；`Esc` 返回原 transcript anchor 与 focus |

Resize 只能改变 placement，不得清除 selected tool、facet、scroll、follow 或已加载 detail。关闭后 focus 回到打开 Inspector 的 entry header；该 entry 已不存在时回到最近有效 entry，再退回 composer。

### 10.2 Header、facets 与 footer

```text
┌─ Edit · Done · 37ms ─────────────────────────────── [×]
│ Overview  Input  Output  Diff  Metadata
├────────────────────────────────────────────────────────
│ src/lib.rs                                      +12 −3
│ …
├────────────────────────────────────────────────────────
│ 1/4 tools · Complete · ↑↓ scroll · ←→ tab · Esc close
```

固定 header 显示：

- lifecycle + symbol；
- human title；
- requested/canonical/effective identity；delegated requested target 与 wrapper 仅作次级 metadata，并明确 verified/legacy；
- origin/Agent/duration；
- close control 与上一项/下一项导航。

按可用数据动态提供 facets：

| Facet | 内容 |
| --- | --- |
| `Overview` | 安全摘要、状态时间线、主要对象、result counts、风险/审批结果、完整性 |
| `Input` | 已脱敏、结构化的 effective input；必要时同时显示 requested wrapper input；legacy 只有 raw input 时必须明确标记，不能冒充 effective |
| `Output` | 完整可用 output 或分页内容；明确 Complete/Truncated/Unavailable |
| `Diff` | 结构化文件列表、hunks、`+N/-N`；不得从普通摘要伪造完整 patch |
| `Log` | stdout/stderr/structured stream；只有协议明确区分时才分区 |
| `Subagent` | nested transcript、tool timeline、result、cancel/failed 状态 |
| `Workflow` | phase/agent timeline、run ID、结果与可用日志 |
| `Resources` | MCP resource、artifact metadata、可用 link/URI；全部脱敏 |
| `Raw` | 已脱敏、限深、不可执行的结构化值；默认不选中 |
| `Metadata` | tool key、requested/canonical/delegated/effective name、wrapper、origin、identity verification、revision、truncation、approval provenance |

默认 facet 的 exact-tool 事实源是 §12.1 覆盖矩阵；本节只定义 family fallback：

- `Edit`/`Write`/`SandboxWrite` → `Diff`（不可用则 `Overview`）；
- `Bash` → `Log`；
- `Agent`/`AgentResult` → `Subagent`；
- `Workflow` → `Workflow`；
- `mcp_read_resource`/`artifact` → `Resources`；
- `TodoWrite`/`SkillTool`/`goal`/`cron_register`/`cron_remove` → `Overview`；
- `AskUserQuestion` 不走普通 Inspector，使用 durable interaction surface；
- 其他 known tool：running → `Input`，completed → `Output`；
- unknown → `Overview`，由用户进入 `Input/Output/Raw`。

Footer 固定显示：

- 当前 facet、scroll/follow 状态；
- truncation/completeness；
- 可用键位；
- detail loading/error/retry 状态。

### 10.3 加载、错误与一致性

- 打开具有 remote detail 的 Inspector 时先呈现已有 summary/preview，再异步加载，不显示空白整面；
- 加载失败保留 Inline 审计记录，并在 Inspector 显示原因与显式 Retry；
- 切换 tool/facet 时使用 generation 丢弃迟到响应；
- 同一 revision 的响应可缓存，key 至少包含 `session_id + ToolKey + facet + revision + cursor`；
- session clear/rewind/switch/close 后清理相应 cache 和 pending request；
- running facet 更新时，处于 FollowBottom 才吸底；用户向上滚动后保持位置并显示 `New output`；
- Inspector 不得自动修改 transcript fold、follow 或 selection。

### 10.4 与现有 Panel/Popup 的边界

**当前基线**：现有 Panel/Popup 已提供若干可复用 primitives，但不是目标 Inspector。

- border/theme primitives；
- `AreaTracker`、list layout、scroll throttle 与 scrollbar 仲裁；
- SubAgent Detail 和 Workflow Panel 的内容 renderer；
- Esc 关闭与 focus restoration 模式。

**目标规范**：不得直接把 Inspector 实现成普通 `PanelKind`：

- 当前 Panel 是静态 registry + 互斥栈，Inspector 是动态 selected tool + facet + revision；
- 当前 Panel 只支持全宽 bottom drawer，并会隐藏 composer；
- 当前 `mouse_router` 将 Panel 视为全局遮挡，不支持 dock 与 transcript 并列操作；
- 管理型 Panel（MCP、Cron、Workflow、Tasks 等）仍由用户主动打开，不应被单次工具详情抢占。

Popup 的定位和最高 focus priority 可继续用于 Modal，但 Inspector 不得使用 Popup，否则普通详情会阻塞整个 workspace。

## 11. Decision Modal 与 durable interaction

**目标规范**：Modal 只用于阻塞决策；下述交互和安全不变量覆盖 HITL、AskUser 与 OAuth 等流程。

### 11.1 通用 Modal anatomy

```text
┌─ Permission required ─────────────────────────────────┐
│ MCP · github · Create issue                           │
│ via ExecuteExtraTool                                  │
│                                                      │
│ Repository: owner/repo                                │
│ Title: Fix rendering bug                              │
│ Risk: sends data to an external MCP server            │
│                                      body scroll  ▐    │
├───────────────────────────────────────────────────────┤
│ [Review details]   [Deny]   [Allow once]              │
└───────────────────────────────────────────────────────┘
```

- header、可滚 body 与 sticky action footer 分离；
- 默认 focus 落在最小权限动作（通常 `Deny`），不得默认批准；
- `Tab/Shift+Tab` 在可操作项间循环；Modal 内限制 focus；
- `Review details` 可打开 Inspector，但 pending decision 必须保留；返回后恢复 Modal focus；
- 背景键盘、鼠标和滚动完全 inert；
- Narrow/低高度时 Modal 全屏，风险对象与安全动作必须在首屏可达；
- 只有协议实际支持的 action 才显示；当前若只有 allow-once/reject，不得虚构 `Always allow`。

### 11.2 审批安全不变量

**当前基线**：当前整窗 `MouseDown` 即批准的路径不符合目标规范。

**目标规范**：实现必须首先移除此路径，并满足：

1. `Down` 只记录 `PressedTarget`，不立即执行 action；
2. `Up` 与 `Down` 必须命中同一 enabled action、同一 request revision，且未升级为 drag；
3. 点击标题、正文、边框、空白、scrollbar 或遮罩不得批准；
4. 指针按下按钮后移出再释放不得提交；
5. 提交后进入 `Submitting…`，禁用所有 action，重复点击/Enter 只发送一次；
6. response 失败时保留 Modal 与 durable interaction，显示可重试错误；
7. stale `request_id/tool_id/revision/frame_generation` 必须 no-op；
8. 关闭 Modal 时消费本次 release，防止 click-through 到 transcript；
9. batch approval 明确列出每个 effective action 与作用范围；混合高风险批次不提供模糊的一键批准；
10. `ExecuteExtraTool` 必须展示 effective tool 与 effective args，不能只展示 wrapper；
11. `Agent` 审批需明确披露委派边界：子 Agent 内部工具不经过父级逐工具 HITL；
12. Bypass/AcceptEdits 等 permission mode 只展示后端事实，UI 不自行推断或放行。

### 11.3 AskUserQuestion

`AskUserQuestion` 不按普通 Generic tool card 渲染，而是 durable interaction block：

- transcript 中保留问题摘要、pending/resolved 状态和最终选择；
- 单题、短选项可 inline 操作；
- 多题、多选、自定义输入或长说明进入响应式 form：Wide/Standard 可用 drawer，Narrow 使用全屏页；
- form 的选项有明确 hover/focus/pressed 状态，整行 click 等价于聚焦该 option，不直接提交整个表单；
- Submit 是独立 action，只有所有 required question 有效时启用；
- 自定义文本编辑使用独立 input focus，普通 transcript 快捷键不得截获输入；
- resolved 后 block 默认收束为带答案摘要的 `Collapsed result`，展开后仍显示完整问题、选项与回答，replay 后仍可辨认。

### 11.4 BrowseHistory 中的 pending interaction

原设计中“BrowseHistory 不移动 viewport”与“pending interaction 强制锚定”冲突，目标规则统一为：

- `FollowBottom` 下，新 interaction 滚入视口并可自动打开 decision surface；
- `BrowseHistory` 下不得抢动 viewport，只显示 sticky `Approval required` / `Question waiting` indicator；
- click indicator 或执行对应 jump 命令后才定位 interaction；
- 持续 streaming 不得反复抢回 approval；
- pending interaction 不得因 tool grouping 或普通折叠而不可发现。

## 12. 每个工具的新展示规范

**目标规范**：本节定义 §4.1 每个 callable tool 与动态边界的精确展示；当前仅已实现的 Skill/Todo 和局部 renderer 特判仍以 §3.2 为准。

### 12.1 完整覆盖矩阵

下表逐项覆盖当前静态工具与动态边界。表中 Inline、Preview、Inspector 与交互列均为 **目标规范**；如果单元格引用现状或旧历史，会在句内另标 **当前基线** 或 **兼容回放**。`Inspector` 表示协议可达的完整或分页详情；数据不可用时必须显示原因，而不是伪造。

| 工具 | **目标规范**：Inline（Collapsed） | **目标规范**：Preview / Expanded | **目标规范**：Inspector 默认 facet | **目标规范**：交互与风险提示 |
| --- | --- | --- | --- | --- |
| `Read` | `Read {path} · {line_count/range}` | 首段 excerpt，保留行号 | Output | Copy path/location；敏感文件只显示已脱敏内容或 unavailable |
| `Write` | `Wrote {path} · +N −M` 或 `Created {path}` | 首个 diff hunk | Diff | 当前策略通常需审批；不得显示完整 input content 于 Inline |
| `Edit` | `Edited {path} · +N −M` | 首个 diff hunk / conflict | Diff | 当前策略通常需审批；冲突保留 error summary |
| `Glob` | `Matched "{pattern}" · N files` | 首批 paths | Output | Copy pattern/path；不重复 dump file list |
| `Grep` | `Searched "{pattern}" · N matches in M files` | 首批命中与 context | Output | Copy query/location；显示 filters/range 于 Metadata |
| `folder_operations` | 按 operation 显示 `Listed/Checked/Created/Scanned {path}` | 首批 entries 或 existence result | Output | `create`/未来 mutation 显示 effect；UI 服从后端审批，不自行判定 |
| `SandboxWrite` | `Wrote {path} · sandbox` | diff/结果摘要 | Diff | 明确 allowed sandbox scope；兼容别名 `WriteSandbox` |
| `Bash` | `Ran {command} · running/exit N/bg` | stdout/stderr tail 或完成摘要 | Log | command 脱敏；当前策略通常需审批；cancel 只在后端能力存在时显示 |
| `WebFetch` | `Fetched {host/path} · status/bytes/lines` | 安全 excerpt | Output | 标记 network；URL userinfo/query secret 脱敏 |
| `WebSearch` | `Searched web for "{query}" · N results` | 首批 title + host | Output | 标记 external content；当前策略通常需审批 |
| `artifact` | `Published {file} · public · {ttl}` | sanitized link metadata | Resources | 始终醒目标记 public upload 与 expiry；当前 UI 不擅自增加审批 |
| `TodoWrite` | `Tasks {done}/{total} · {active}` | 本次 changes，不重复全量 output | Overview | 语义卡；完整列表进入 Inspector/Tasks 管理面，不生成重复 Generic dump |
| `AskUserQuestion` | `Question waiting · N prompts` / resolved result | inline options（适用时） | 不走普通 Inspector | durable interaction + form/Modal，详见 §11.3 |
| `SkillTool` | `Loaded skill {name}` | skill purpose/source metadata | Overview | 不默认展示完整 `SKILL.md` 或内部 prompt；兼容旧名 `Skill` |
| `DiscoverSkillsTool` | `Found N skills for "{query}"` | 首批 name + source | Output | 可进入完整候选；不得把 skill body 混入列表 |
| `Agent` | `Agent {type/name} · {task} · N tools · status` | 最近 activity/result/error | Subagent | 显示 sync/background/fork/resume/model；当前策略通常需审批；兼容大小写无关别名 `task` 与历史 `Task` |
| `AgentResult` | 合并为对应后台 Agent 的 `Result received`；无法关联时独立一行 | result 摘要 | Subagent | 标记 synthetic；不提供“再次调用 AgentResult”动作 |
| `SearchExtraTools` | `Discovered N tools for "{query}"` | 首批 title + namespace | Output | 展示 direct/deferred/provenance metadata；不 dump schema 全文于 Inline |
| `ExecuteExtraTool` | verified snapshot 下以 effective tool 为主并尾注 `via ExecuteExtraTool`；legacy 显示 `Requested {target} via …` | verified 时委托 target presenter；否则 wrapper 安全摘要 | verified effective facet / Overview | 权限与风险只跟随 policy/verified target；raw wrapper 不得冒充 resolved target；malformed input 安全降级 |
| `cron_register` | `Scheduled {expression} · {task_id/next fire}` | prompt 的脱敏摘要 | Overview | 标记 recurring delegated action；当前策略需审批 |
| `cron_list` | `Listed N schedules` | 首批 task、expression、next fire | Output | 完整列表可跳转 Cron Panel；prompt 脱敏 |
| `cron_remove` | `Removed schedule {short_id}` | result/error | Overview | 明确 destructive effect；是否审批服从后端 policy |
| `LSP` | `{operation verb} {path:line/query} · N results` | 首批 location/diagnostic | Output | location 可语义复制；按 severity/symbol 结构化展示 |
| `Workflow` | `Workflow {name/run_id} · {phase/status} · N agents` | 当前 phase 与 agent 摘要 | Workflow | 异步状态进入 activity indicator；script 默认隐藏；cancel 需后端能力与确认 |
| `goal` | `Goal {action} · {objective/status}` | state transition/evidence 摘要 | Overview | `complete/block/clear` 使用明确动词；不与 Todo footer 重复 |
| `mcp_read_resource` | `Read MCP resource · {server} · {sanitized_uri}` | content type/size/excerpt | Resources | server/URI 明确；resource body 视为不可信外部内容 |
| `mcp__{sanitized_server}__{sanitized_tool}` | `MCP · {server} · {Friendly Tool} · {safe object}` | allowlist fields / safe result preview | Overview/Output | 当前策略通常需审批；原始 server/tool provenance 优先来自 snapshot；unknown effect 不标为安全 |
| 未知 plugin/future tool | `{Friendly Title} · {status} · N parameters` | 安全 allowlist preview 或 unavailable | Overview | 仅导航、折叠、脱敏 copy；不猜风险、不提供副作用快捷动作 |

### 12.2 Read / Glob / Grep

```text
   ✓ Read       peri-tui/src/kit/message_area/render.rs · 184 lines   37ms
   ✓ Matched    **/*.rs · 126 files
   ✓ Searched   "ToolEnded" · 14 matches in 6 files
```

- path、pattern、query 是主对象，result count 是次级结果；
- `reading` 及其大小写变体作为 `Read` 的 raw alias event 进入同一 presenter；
- `Read` 显示明确 range/page（若输入存在），避免把局部读取误解为完整文件；
- `Glob` preview 只列路径；`Grep` preview 保留 location + 一行 context；
- Inspector 提供 filters、glob/type、context、offset 与 truncation metadata；
- 超长单行按 Unicode display width 截断，完整语义内容仅在详情可达时提供；
- 敏感路径按策略显示 `Content hidden`，不得因用户打开 Inspector 绕过上游限制。

### 12.3 Write / Edit / SandboxWrite

```text
   ✓ Edited     src/render.rs · +12 −3                  84ms
   ! Wrote      .peri/plans/ui.md · sandbox
   × Edited     src/render.rs · conflict
```

- `Write` 根据已知结果选择 `Created` 或 `Wrote`；无法确认时使用中性 `Wrote`；
- `Edit` 使用 `Edited`，error 时保留 conflict/old_string not found 等安全摘要；
- `SandboxWrite` 显示 `sandbox` badge 与 allowed root，避免误认为普通项目写入；
- Expanded 只显示首个 hunk 与有界 change lines；
- Inspector Diff 显示文件列表、hunk 导航、行号与 `+/-` 标记；code 默认不软换行；
- 如果上游仅提供计数，显示 `Full diff was not provided`，不得从摘要构造 patch；
- copy diff 复制 patch 标记与正文，不复制 border、focus marker 或行号 gutter；
- 不提供历史调用“一键 undo/rerun”。

### 12.4 folder_operations

`folder_operations` 是一个多 operation 工具，不能统一显示为模糊的 `Folder`：

| operation | Verb | Primary object | Result |
| --- | --- | --- | --- |
| `list` | `Listed` | folder path | entry count |
| `exists` | `Checked` | path | exists / missing + file/folder type |
| `deep_scan` | `Scanned` | folder path | entry count + depth |
| `create` | `Created folder` | folder path | created / already existed |
| future mutation | 从 descriptor/effect 推导中性动词 | canonical path | explicit effect + policy result |

Unknown operation 不得显示成功暗示，使用 `Folder operation {name} · status`。审批状态完全服从后端 policy；UI 可以展示 effect，但不能自行放行或阻止。

### 12.5 Bash

```text
   ◐ Running    cargo test -p peri-tui                         4s
     test message_area::scroll ...
     test message_area::selection ...

   ✓ Ran        cargo test -p peri-tui · exit 0              8.4s
```

- Inline command 脱敏并保持单一逻辑行；显示 cwd 只在非 session cwd 时有价值；
- `Shell` 及其大小写变体作为 `Bash` 的 raw alias event 进入同一 presenter；
- running preview 是有界 tail，不得在 transcript 无限增长；
- `run_in_background=true` 显示 `background` 与 task ID；完成结果应与同一 task/Agent activity 关联；
- Inspector Log 可按协议提供 `All / stdout / stderr` facets；不能区分时只显示 `Output`；
- 显示 exit code、timeout、cancel、duration 与 truncation；
- 用户滚离 Log 底部后停止吸底并显示独立 `New output`；
- cancel 必须经过 ACP/Agent 现有执行权，且是显式确认动作；
- 不提供一键 rerun、命令编辑或自动打开 shell。

### 12.6 WebSearch / WebFetch / artifact

```text
   ✓ Searched web   "ratatui mouse events" · 10 results
   ✓ Fetched        docs.example.com/guide · 200 · 142 KB
   ✓ Published      report.md · public · expires in 7d
```

- URL 展示 host + 安全 path；移除 userinfo，敏感 query value 统一 `[redacted]`；
- WebSearch preview 使用 `title · host · excerpt`，不得将外部内容伪装为 Agent 结论；
- WebFetch 明确 status/size/content type 与 truncation；
- artifact 始终展示 `public upload`、文件名、TTL 与 sanitized share URL metadata；
- 点击 URL 默认只聚焦 semantic target；复制或打开外部浏览器必须是显式 action，且没有 host capability 时只提供 Copy；
- 外部内容中的 ANSI/OSC/control sequence 在进入 ViewModel 前清理。

### 12.7 SkillTool / DiscoverSkillsTool

```text
   ✓ Loaded skill   diagnosing-bugs · user
   ✓ Found skills   "testing" · 4 matches
```

- SkillTool 使用名称与 source 作为主要事实；body 是模型内部工作材料，不默认铺入用户 transcript；
- Expanded 可展示 description、source 与 loaded 状态；
- Discover preview 展示 name、description、source，完整候选进入 Inspector；
- 同名覆盖/来源优先级只在 Metadata 中解释，不用长文本污染 header；
- 历史 `Skill` 名称映射为同一 presenter。

### 12.8 SearchExtraTools / ExecuteExtraTool

```text
   ✓ Discovered     "slack send" · 3 tools
   ! MCP · slack    Send Message · #release                 approval
                    via ExecuteExtraTool
```

- SearchExtraTools 显示 query、结果数与首批 title/namespace；Inspector 可展示 description/schema，但必须限深、限长；
- ExecuteExtraTool 不拥有独立视觉家族，它是调用 provenance：
  1. legacy projection 只能从 raw wrapper input 读取并脱敏 `tool_name`/`params`，标为 delegated requested target；
  2. 只有 resolver identity snapshot 才能确认 canonical target、normalized/effective params 与 `target` presenter；
  3. verified 时使用 target presenter、icon、facet；次级 metadata 显示 `via ExecuteExtraTool`；
  4. approval 只展示 broker/policy 提供的实际名称与输入；若 snapshot 缺失，不得把 raw wrapper 参数标成“已批准的 effective args”；
  5. malformed wrapper 显示 `Execute extra tool · invalid target`，不 dump raw object。
- `SearchExtraTools` 与 `ExecuteExtraTool` 之间不要求自动视觉配对；每次调用仍有独立审计 entry。

### 12.9 Agent / AgentResult

```text
   ◐ Agent explorer   Inspecting message flow · 6 tools · background
   ✓ Agent explorer   Found 8 UI patterns · 12s
   × Agent coder      Failed to build · permission denied
```

- header 优先显示 `name`/`subagent_type`、description/task 摘要、sync/background/fork/resume、model 与 tool count；
- prompt 可能很长，仅以安全短摘要展示；完整内部 prompt 默认不进入 Inspector；
- Enter/Details 打开 Subagent facet，不把 nested transcript 铺入主时间轴；
- 并行 Agents 各保留独立 identity，group 可显示完成/失败计数；
- `AgentResult` 应通过 task/child identity 合并到原 Agent entry；无法关联时显示独立 legacy result entry，不得伪装成真实 tool call；
- background completion 到达 BrowseHistory 时增加 activity indicator，不抢 transcript；
- cancel 仅对真实可取消任务显示，须确认 scope；
- approval 明确显示 delegation trust boundary，尤其是子 Agent 内部没有父级逐工具审批。

### 12.10 Workflow

```text
   ◐ Workflow ui-audit   Phase 2/4 · 2 agents running · run 019f…
   ✓ Workflow ui-audit   4 phases · 6 agents · 42s
```

- Inline 显示 name/run ID 短值、phase、agent count、running/completed/failed；
- Expanded 显示当前 phase 和 Agent 摘要；
- Inspector Workflow facet 复用现有 Workflow Panel 的 timeline/agent renderer；
- inline script 默认隐藏，仅展示 scriptPath/name/args 摘要与安全 metadata；
- resume 显示来源 run ID；
- Workflow 是异步执行，启动工具成功不等于整个 workflow 成功，必须区分 `Launched` 与最终 `Completed/Failed`；
- cancel/kill 若后端支持，使用显式确认，不得通过双击或 context shortcut 直接执行。

### 12.11 TodoWrite / goal

```text
   ◐ Tasks       3/7 · Running tests
   ✓ Goal        Completed · Mouse routing is region-owned
   ! Goal        Blocked · ACP detail endpoint unavailable
```

- TodoWrite 只显示 semantic changes 与当前进度，不同时渲染 Generic output 和重复 footer；
- 当前 in-progress item 可进入 activity indicator；最终 assistant answer 不被 Todo footer 隔断；
- goal 按 `create/complete/block/clear/get` 使用 `Created/Completed/Blocked/Cleared/Read goal`；
- objective、evidence、missing reason 使用有界摘要；
- Todo/Goal 的完整状态进入 Inspector 或既有管理 Panel，不在主时间轴无限展开；
- UI 是观察面，不直接编辑 Agent 所有的 Todo/Goal 状态。

### 12.12 Cron

```text
   ! Schedule      */30 * * * * · "Check CI…"              approval
   ✓ Scheduled     task a1b2c3d4 · next 14:30 UTC
   ✓ Listed        4 schedules
   ✓ Removed       task a1b2c3d4
```

- register 在审批前显示 expression、prompt 安全摘要与 recurring delegated-action 警告；
- completed 显示 short task ID 与 next fire；
- list preview 显示 ID、enabled、expression、next fire，不在 Inline 打印完整 prompt；
- remove 使用明确 destructive verb；审批与否服从后端 policy，UI 仅准确展示；
- Details 可跳转到 Cron management Panel，但单次调用的审计 entry 永久保留。

### 12.13 LSP

```text
   ✓ References    src/lib.rs:42 · 18 locations
   ! Diagnostics   src/main.rs · 2 errors · 3 warnings
   ✓ Symbols       "ToolEntryVm" · 6 results
```

- operation 映射为稳定 human verb；
- path + line/character 或 query 是主要对象；
- diagnostics 使用 severity symbol + text + count，不只靠红黄颜色；
- Expanded 展示首批 location/diagnostic；Inspector 提供完整列表、message、source、related locations；
- location 可语义复制；外部编辑器打开不是默认动作；
- LSP server 未就绪/无适配 extension 使用明确 recovery hint。

### 12.14 MCP resource 与动态 MCP tool

```text
   ✓ MCP · docs       Read resource · resource://guide · 18 KB
   ! MCP · github     Create issue · owner/repo              approval
```

- `mcp_read_resource` 显示 server 与 sanitized URI，Inspector Resources 显示 content type、size、completeness 和脱敏内容；
- `mcp__{sanitized_server}__{sanitized_tool}` 的两个 name component 只保留 `[A-Za-z0-9_-]`，其他字符替换为 `_`；仅从该模型名反解析不能恢复原始 server/tool，因此只作 legacy label，不作为完整 provenance 或安全放行依据；
- 如果调用时 descriptor 可用，冻结 title、namespace、schema 与 origin snapshot；
- Generic MCP primary object 只从安全 allowlist 提取：`file_path/path/url/query/pattern/name/id/operation/uri`；
- 未知 field 只显示参数数量，Raw facet 仍必须脱敏、限深、限长；
- MCP output 一律视为不可信外部内容，移除控制字符；
- 当前 policy 对 `mcp__*` 通常要求审批；UI 展示 policy 的真实结果，不根据前缀自行决定。

### 12.15 安全 Generic fallback

任何未知、plugin 或未来工具至少获得：

```text
   ◐ Friendly Tool Name · Running · 5 parameters
   × Friendly Tool Name · Failed · Details unavailable
```

规则：

1. title 优先使用调用时 `ToolDescription.title`，其次安全地从 name 派生；
2. namespace/origin 只作 provenance；
3. Inline 主对象仅从字段 allowlist 提取，值先脱敏；
4. 未命中 allowlist 时只显示参数数量；
5. output 显示安全 status/count/excerpt，不把任意 object 序列化到 header；
6. Unknown effect 不显示 `safe`、`read-only` 等暗示；
7. 只提供 focus、fold、Inspector、脱敏 copy；不提供 rerun、undo、approve shortcut 等猜测动作；
8. malformed input/output 不 panic，显示 `Invalid structured data` 与 Metadata；
9. 未来新增工具即使没有专属 presenter，也必须通过 fallback 验收。

## 13. 统一鼠标、焦点与命令模型

### 13.1 InteractionFrame / HitMap

每个完整渲染帧必须产出只读语义命中快照，而不是在事件到达时重新猜测坐标：

```text
InteractionFrame {
  frame_id,
  layout_generation,
  viewport_generation,
  regions: [HitRegion],
}

HitRegion {
  target_id,
  role,
  rects,
  z_index,
  scroll_owner?,
  enabled,
  commands,
}
```

- `target_id` 使用稳定 semantic identity，例如 `ToolHeader(ToolKey)`、`ToolDetails(ToolKey)`、`ApprovalOption(request_id, option_id)`；
- 折行 target 可以拥有多个 rect；
- 只为可见 viewport 与前景 surface 生成 region，不扫描完整历史；
- event 只消费最近完成帧；frame/layout generation 不匹配时取消 pending gesture；
- renderer 负责注册几何与语义，command router 负责执行业务动作；
- keyboard 与 mouse 最终都生成同一套 `UiCommand`，避免两条逻辑长期漂移。

命中优先级从高到低：

1. Modal action / body / shield；
2. anchored popover / completion；
3. Inspector 或 active management Panel 的 control、scrollbar、body；
4. transcript 显式 action（Details、Copy、interaction option）；
5. transcript scrollbar；
6. disclosure；
7. entry header；
8. selectable body；
9. transcript background。

同 z-index 内优先选择面积更小、语义更具体的 target。任一前景 Modal 命中 shield 时必须消费事件，禁止背景 click-through。

### 13.2 Pointer gesture 状态机

```text
Idle / Hovered
  └─ LeftDown → Pressed { target, frame, origin }

Pressed
  ├─ Move/Drag 未超阈值              → Pressed
  ├─ 超阈值 + selectable body         → SelectingText
  ├─ 超阈值 + scrollbar thumb         → DraggingScrollbar
  ├─ Up 命中同 target/frame           → Activate
  ├─ Up 坐标超阈值或 target 不同      → Cancel
  └─ resize/occlusion/generation stale → Cancel

SelectingText      ─ Drag* → update ─ Up → finalize/copy
DraggingScrollbar  ─ Drag* → scroll ─ Up → release capture
```

点击判定：

- `Down` 与 `Up` 坐标差不超过 2 列、1 行；
- 即使终端漏发 `Drag`，只要 `Up` 超出阈值也不得解释为 click；
- pointer capture 后，直到 Up/Cancel 只由 capture owner 接收；
- Popup/Modal 关闭时消费触发关闭的 Up，防止透传；
- resize、session reset、rewind、scroll 或目标移出 viewport 可取消 press；
- double-click 不承载批准、取消、重跑等任何副作用。核心功能不依赖 double-click。

### 13.3 Hover

- `MouseEventKind::Moved` 只有在 semantic target 变化时更新 hover state；同一区域内移动不触发 redraw；
- hover 不移动 keyboard focus、不改变 fold、不发网络请求；
- header hover 显示 disclosure、focus rail 或 `Details` hint，但不得改变 entry 高度或主文本起点；
- action hover 使用与 keyboard focus 同一语义 token，keyboard focus 必须更明显；
- hover 不揭示 secret、完整参数或 tooltip-only 信息；
- Reduced/SSH profile 可关闭连续 hover，保留 click 和全部键盘能力；
- 终端不发送 Moved 时，所有 action 仍通过 focus hint、固定 disclosure 或键盘帮助可发现。

### 13.4 Click 语义

| Target | 单击行为 | 不得发生 |
| --- | --- | --- |
| entry header | 设置 transcript focus；若 entry 可折叠，等价 focused `Enter` | 不提交 interaction，不打开 URL，不重跑 |
| disclosure | 切换 Collapsed/Preview/Expanded | 不改变 Inspector selection，除非用户再点 Details |
| body | 只设置 entry focus；Down+drag 进入文本选择 | 不因正文 click 展开或批准 |
| `Details` | 打开 Inspector 并聚焦；保持 transcript anchor | 不打开 Modal |
| semantic path/URL | 聚焦 target；action menu 提供 Copy/Open（capability 存在时） | 不在首次单击自动启动外部程序 |
| scrollbar | track click / thumb drag，仅滚所属 region | 不开始文本选择 |
| interaction option | 选择该 option；提交规则由 interaction 类型定义 | 不以整块背景 click 代替 Submit |
| Modal action | 同 target Down+Up 后激活 | 不在 MouseDown 激活 |
| sticky indicator | 跳到 pending/unseen entry 或打开过滤详情 | 不丢失 BrowseHistory anchor 的返回位置 |

可选右键打开 semantic action menu，但必须有 `Shift+F10` 或 focused `a` 等键盘等价；不支持右键的终端不损失功能。action menu 只包含当前 target 声明的安全命令，例如：

- Open details；
- Expand/Collapse；
- Copy summary/path/location/redacted input/output；
- Jump to parent/child entry；
- Open corresponding management Panel；
- 对真实可取消后台任务显示 `Cancel…`，随后进入确认。

菜单不得提供直接 approve、rerun、undo、永久放行或绕过权限模式。

### 13.5 FocusOwner 与恢复

**当前基线**：entry 级键盘 focus 已有 `FOCUSED_ENTRY` atom 并驱动 header/body 键位，但尚未成为统一的 `FocusOwner` 层级；下述 focus 栈、恢复顺序与 Modal 限制均为 **目标规范**。

目标 focus 层级：

```text
DecisionModal
  > InlineCompletion / AnchoredPopover
  > ToolInspector or ActivePanel
  > Transcript
  > Composer
```

- 默认 focus 在 Composer；
- transcript header/body click 或 `Alt+Up/Down` 进入 Transcript；
- 点击 composer 立即回到 Composer；
- `Details`/focused `d` 打开 Inspector 并保存来源 focus；
- management Panel 与 Inspector 共享同一 workspace detail slot；打开 Panel 时暂存 Inspector，关闭后可恢复；
- Modal 打开时 push 当前 focus，关闭后恢复仍有效的 target；
- Modal 限制 focus；Inspector/Panel 不限制全局 Tab，但只有拥有 focus 时处理其键盘滚动与 tab；
- focus target 被 streaming/grouping/rewind 删除时，按同组邻项→邻近 entry→Composer 的顺序恢复；
- hover 永远不覆盖 focus。

`NO_COLOR` 下 focus 使用 outer `>`、反色或 underline 等非颜色线索。

### 13.6 键鼠等价

| 语义动作 | 键盘 | 鼠标 |
| --- | --- | --- |
| 聚焦前/后 entry | `Alt+Up/Down` | 单击 header/body |
| Collapsed↔Expanded | focused `Enter` | 单击 header/disclosure |
| 切换 Preview | focused `Space` | 单击 preview control |
| 打开 Tool Inspector | transcript focus 下 `d` 或 focused Details + `Enter` | 单击 `Details` |
| Inspector facet | `Left/Right` 或 `Tab` 到 tab 后 `Enter` | 单击 tab |
| 激活显式 control | `Enter`/`Space` | 同 target Down+Up |
| transcript 滚动 | 既有 `Ctrl+Up/Down/Home/End`；导航模式可用 PgUp/PgDn | transcript 内 wheel/scrollbar |
| Inspector/Panel 滚动 | surface focus 下 `Up/Down/PgUp/PgDn/Home/End` | 对应区域 wheel/scrollbar |
| 恢复 follow | `End` 或 focused indicator `Enter` | 点击 `New output` |
| Copy semantic section | focused `c`/action menu | Copy action 或文本 drag |
| Action menu | focused `a` 或 `Shift+F10` | 右键（可选）/显式 More |
| 关闭一个层级 | `Esc` | Close/Cancel control |

单字母命令只在 Transcript/Inspector navigation focus 下生效；Composer 文本编辑时不得截获 `d`、`c`、`a` 等输入。

### 13.7 Semantic copy 与 clipboard feedback

- 复制语义内容，不复制 accent、bullet、chevron、focus rail、border、scrollbar、button chrome；
- code 不复制语言标签和行号；diff 保留 patch marker，忽略视觉 gutter；
- Header copy 输出 `{Verb} {primary object} · {result}`；
- path/location/command/URL 可作为独立 semantic target 复制；
- 所有 copy 都使用已脱敏数据；敏感 Raw 不提供未脱敏复制；
- grapheme、CJK、emoji、combining mark 按 terminal display width 计算 hit/selection；
- 系统 clipboard 不可用时不得静默失败：显示 transient notification，并保留 bounded internal copy buffer；
- OSC 52 如未来启用，必须由用户配置、限制长度并继续使用脱敏内容。

## 14. Scroll ownership、follow 与长内容

**目标规范**：本节定义滚动所有权、follow 与长内容处理；与当前实现的差异以 §3/§8.4 基线为准。

### 14.1 ScrollRegion

目标可滚区域：

```text
Transcript
ToolInspector(facet)
ActivePanel(slot)
DecisionModal(body)
AnchoredPopover(list)
```

Expanded tool body 不注册 ScrollRegion；它只显示有界 preview，完整内容进入 Inspector。

路由规则：

1. pointer capture owner 优先；
2. wheel 交给鼠标坐标下 z-index 最高的 ScrollRegion；
3. 键盘滚动交给当前 `FocusOwner` 对应 region；
4. region 到达顶部/底部后仍消费本次 wheel，禁止 scroll chaining 到背景；
5. Modal body 消费 Modal 矩形内全部 wheel；遮罩区域也不得滚背景；
6. Wide dock 下 transcript 和 Inspector 可同时存在：鼠标在哪个区域，就滚哪个区域；
7. 当前全局 `is_occluded()` 需演进为 region-aware routing，不能以“存在 Inspector 即屏蔽整个 MessageArea”实现。

### 14.2 统一滚轮手感

- transcript、Inspector、Panel、Modal 共用同一 scroll intent 与基础步长配置；
- 高频 touchpad/ghostty wheel event 先累积，在渲染帧预算内合并应用，速度不得直接随 event rate 放大；
- 停手后最后一批 pending delta 必须 flush；反向输入不得与陈旧 pending 产生回弹；
- SSH/tmux burst 下允许批量位移，但每帧应有合理上限并保留剩余 delta；
- 滚轮只改变 scroll，不隐式改变 keyboard focus；
- track click 使用 page intent，thumb drag 使用绝对比例；所有 `u16` geometry 饱和计算；
- 不依赖 pixel scrolling；终端只提供离散 wheel 时仍保持一致语义。

### 14.3 Transcript follow

```text
FollowBottom
  └─ 用户向上滚动/拖 scrollbar → BrowseHistory { anchor, unseen_count }

BrowseHistory
  └─ 到达底部 / End / 点击 indicator → FollowBottom
```

- BrowseHistory 中新内容不得移动 viewport，只增加 unseen count；
- resize、group 展开、fold 切换按 `EntryKey + intra-entry visual offset` 恢复 anchor；
- 展开/折叠时尽量保持被操作 header 的屏幕行不变；
- 打开/关闭 Inspector 不改变 transcript follow；
- new output indicator 显示可理解计数，例如 `↓ 12 new lines · 2 tool updates`；
- pending approval/AskUser 在 BrowseHistory 中使用独立 sticky indicator，不抢 viewport；
- turn 完成不强制回到底部。

### 14.4 Inspector follow

Inspector 的 Log/Output/Workflow/Subagent facet 独立维护 follow：

- 初始打开 running stream 时为 FollowBottom；
- 用户向上滚动即进入 detail Browse；
- 新 chunk/revision 只增加该 facet 的 unseen count；
- 点击 `New output` 或 End 恢复；
- 切换 facet 保存各自 scroll/follow；
- 切换 tool 可缓存最近有限数量的 detail positions，超出 LRU 后安全重置顶部/底部；
- transcript 与 Inspector 的 follow 状态不得相互覆盖。

### 14.5 大输出与虚拟化

- Inline/Expanded 保持固定预算，与总输出规模无关；
- Inspector 使用 line index、分页或 viewport virtualization，只渲染可见范围附近；
- 100k 行 output 的 hover、wheel、selection 不得每帧对总行数做线性转换；
- 大 payload 不复制进每个 `TuiRenderUnit` clone；summary 与 detail store/cache 分离；
- 上游落盘结果只通过 opaque ref 读取，不在 TUI 重新创建平行持久化；
- output 被上游截断时显示已显示范围、omitted count（若可知）与 recovery availability；
- 搜索/跳转属于 Inspector 后续能力，不是首阶段前置；如果实现，必须作用于已授权的 detail 内容而非文件系统任意路径。

### 14.6 Tool output streaming（协议依赖）

若要显示真实 running tail，目标事件至少需要稳定 sequence 与 stream type：

```text
ToolOutputChunk {
  session_id,
  ToolKey,
  sequence,
  stream: Stdout | Stderr | Structured,
  chunk,
  detail_revision,
}
```

约束：

- 按 `ARC-EVENT-001` 覆盖 Agent 发射、协议映射、ACP、caps（如适用）与 TUI 消费；
- sequence 去重、保持顺序；迟到 chunk 不复活终态；
- Inline 只更新有界 tail/count，Inspector detail 可分页恢复；
- TUI 没收到 chunk 时只显示 command/input、spinner 与 elapsed，不伪造实时 output；
- cancel、timeout、transport close 与 turn terminal event 必须让 running entry 离开 loading。

## 15. Folding、grouping 与 Activity strip

**目标规范**：本节定义折叠、分组与 Activity strip；当前 `fold_for_status` 行为见 §3.2 与 §15.1 标注。

### 15.1 默认 fold

**目标规范**：下表是 redesign 完成后的默认折叠目标。当前实际 `fold_for_status` 中除 interaction 行（见 §3.3 gap #13，当前仍为 Expanded）外，其余各行的 Running/Succeeded/Error 折叠与下表一致。

| Entry | Running | Succeeded | Error/Denied/Cancelled/TimedOut |
| --- | --- | --- | --- |
| user | Expanded | Expanded/长文折叠 | — |
| assistant | Expanded | Expanded | Expanded |
| reasoning | Preview | Collapsed | Preview |
| tool | Preview | Collapsed | Expanded summary |
| subagent/workflow | Collapsed + live summary | Collapsed | Expanded summary |
| system | Collapsed | Collapsed | Expanded summary |
| interaction | Expanded | Collapsed result | Expanded |

- 用户手动 fold 后，本 turn 内自动策略不得覆盖；
- Inspector 打开与否不影响 fold；
- error 展开仍受行数上限约束，完整内容进入 Inspector；
- `Enter` 切 Collapsed↔Expanded，`Space` 切 Preview；
- user/assistant 等无折叠能力 entry 的 Enter 只设置 focus。

### 15.2 Tool grouping

相邻、Succeeded、低信息密度 tools 可聚合：

```text
   ▸ Inspected 8 files · 3 searches · 11 tools          1.2s
```

不得合并：

- Running、AwaitingApproval、Failed、Denied、Cancelled、TimedOut；
- 当前 focused/hovered/pressed entry；
- 含重要 diff 的 write/edit；
- durable interaction；
- 跨 assistant response、system event、agent/workflow 边界；
- provenance 不同且合并会隐藏外部/MCP 边界的调用。

Group 必须显示总数与非成功数；展开组或 Inspector group list 后每个调用独立可达。Group identity 来自成员稳定 keys，不以当前 vector slot 作为唯一身份。

### 15.3 Activity strip

当 Running、AwaitingApproval、未查看错误或后台 Agent/Workflow 不在当前 viewport 时，composer 上方出现低高度 Activity strip：

```text
! 1 approval   ◐ 3 running   × 1 unseen failure   ↓ 12 new lines
```

- 全部为可聚焦 semantic controls；鼠标 click 与键盘 Enter 等价；
- approval 优先级最高，其次 unseen failure、running、new output；
- 点击/激活后跳到对应 entry 或打开过滤后的 Inspector list；
- FollowBottom 且相关 entry 已可见时不重复展示同一信息；
- Compact 压成一行；Narrow 使用 `!1 ◐3 ×1 ↓12`，focus hint 提供完整文字；
- strip 不常驻显示已完成成功项，不演变成永久 Activity Rail；
- strip 出现/消失必须通过预留 transient-status 区域，避免 composer 与 transcript 大幅跳动。

## 16. 非工具消息与 Composer 连续性

**目标规范**：本节定义非工具 entry 与 composer 连续性；与 §3/§4 的当前基线冲突处以后者为准。

### 16.1 User prompt

```text
│  重新设计整个工具调用体验，并支持友善鼠标交互。
```

- 左对齐，不使用右侧气泡；正文与 tool entries 共用 content 起点；
- 保留换行，长 prompt 使用可控折叠与 `… N more lines`；
- slash、skill token、`@mention` 只做局部强调；
- channel、cron、system-reminder 等带来源事件不得伪装为普通 user prompt；
- body click 只 focus，drag selection 优先。

### 16.2 Assistant response

- assistant 正文是最高视觉权重长文本，不使用完整 card border；
- 同一 message 不按 chunk 创建重复 entry；interleaving 边界仍服从现有 message/tool segment 模型；
- Markdown heading 依靠层级、weight 与空行，不使用彩虹色；
- code block 使用 sunken surface，copy 不含 gutter/语言标签；
- streaming 未闭合 Markdown 使用稳定 plain-text fallback，闭合后再升级；
- tool Inspector 打开时，assistant streaming 仍按 transcript follow 规则工作，不抢 Inspector focus。

### 16.3 Reasoning

```text
   ◐ Thinking… · 8s
     正在检查工具注册与事件路径……

   ▸ Thought for 12s · 14 lines
```

- running 显示最近 2–4 行有界 tail；completed 默认折叠；
- reasoning 权重始终低于最终回答；
- 空 reasoning 仍显示 `Thinking…`；
- 用户手动展开或 BrowseHistory 后不得强制折叠/抢 viewport；
- reasoning 不进入 Tool Inspector。

### 16.4 System event

```text
   ── Context compacted · 18k → 7k ─────────────────────
   !  Permission mode changed to Default
   ×  Connection lost · retrying in 3s
```

- compact、model/session switch、transport、permission mode 等使用明确来源和状态；
- error 提供恢复动作或下一步；
- system event 不伪装成 assistant；
- 可操作 system event 同样注册 semantic region，并遵守键鼠等价与 Down+Up 规则。

### 16.5 Composer 与队列

- composer 的 prompt 起点继续与 transcript content 列对齐；
- Tool Inspector dock/drawer 打开时 composer 保持可用；Narrow 全屏详情例外，Esc 返回 composer/transcript；
- loading 期间提交的新 prompt 显示为 composer 邻接 queue chip，不伪装成已发送 user entry；
- `Esc` 只关闭当前最上层；Modal→popover→Inspector/Panel→selection 的层级必须确定；
- transcript navigation 单字母命令不得污染 composer editing；
- Activity strip、new-output indicator 与 queue 使用固定 transient-status 区域，避免布局跳跃。

## 17. 响应式、低高度与性能降级

**目标规范**：本节定义断点、低高度与性能降级策略；当前 `GridSpec` 断点继承关系见 §3.2。

### 17.1 Width breakpoints

延续现有断点：

| Breakpoint | 宽度 | Transcript | Tool row | Inspector |
| --- | --- | --- | --- | --- |
| Wide | `>=100` | content 最大 100 cells，metadata 可右对齐 | 一行完整 verb/object/result/action | 只有终端总宽度 `>=120` 且左侧可保留 60 cells 时右 dock |
| Standard | `60–99` | 默认 gap，metadata 紧跟 summary | 一行或一条次级 metadata | bottom drawer |
| Compact | `40–59` | gap 缩为 1 | 隐藏非关键 duration/provenance，最多两行 | 全屏 detail page |
| Narrow | `<40` | accent 退化为 bullet | status + verb + 主对象；actions 纵向 | 全屏 detail/decision page |

内容压缩优先级：

```text
status > verb > primary object > risk/error/result > action > duration > provenance
```

- path 使用中间省略，优先保留文件名与最后目录；
- command/query 使用尾部 ellipsis，不以字节切断 Unicode；
- Narrow 中 action 不得被裁掉；需要时改为一项一行；
- 完整语义 copy 不受屏幕截断影响，但仍使用脱敏数据。

### 17.2 Height breakpoints

延续现有 `layout_plan`：

- `h >= 12`：完整状态栏；
- `8 <= h < 12`：精简状态栏与 title；
- `h < 8`：隐藏非必要 status，composer 钳制，优先保留 transcript。

新增 surface 约束：

- transcript 在普通 workspace 中至少保留 3 行；
- bottom Inspector drawer 不超过 workspace 可用高度的一半；
- 高度不足 12 时详情与 Modal 使用全屏页；
- 低高度 Modal 首屏必须包含风险对象、当前 selection 与至少一个安全 action；
- sticky action footer 优先于非关键 metadata；
- `40×8`、`30×8` 下不得出现超出 viewport 的 border/action，也不得让 approval 不可达。

### 17.3 Performance profiles

```text
Full | Reduced | KeyboardOnly
```

- 不根据网络延迟猜测 profile；SSH 环境可默认建议 Reduced，但允许用户配置；
- Full：target 变化时 hover、正常 spinner、完整鼠标能力；
- Reduced：hover 可关闭或限制为低频 target-change redraw；spinner 静态/低频；wheel/drag event 合并；running elapsed 最多 1Hz；
- KeyboardOnly：不依赖 mouse capture/Moved，保留所有 focus、fold、Inspector、copy、approval 与 close 命令；
- idle 时不得持续高频 redraw；
- Activity strip 按事件增量维护，不每帧扫描完整 transcript；
- Inspector 关闭时不解析大 payload/diff tree；
- 只为 viewport 生成 hit regions；
- animation tick 不触发完整 transcript reflow。

## 18. Accessibility 与终端兼容

**目标规范**：

必须满足：

- lifecycle、risk、focus、selection 由 symbol + text + modifier + color 多重表达；
- `NO_COLOR` 下所有状态和 focus 可辨认；
- Unicode 不可用时使用 ASCII fallback；
- reduced-motion 使用静态 symbol + elapsed/status，不丢状态；
- 无 mouse capture、无 Moved、无 right-click 时功能完整；
- hover 不是 action 的唯一入口；
- keyboard 可完成展开、详情、facet、滚动、复制、审批、提问、关闭；
- focus 顺序稳定，Overlay 关闭后恢复来源 target；
- CJK、emoji、combining mark 的 truncate/hit/selection/copy 坐标一致；
- UI chrome、line number、scrollbar、button border 不进入 semantic copy；
- 新增用户可见文本同步更新 `peri-tui/locales/en/main.ftl` 与 `peri-tui/locales/zh-CN/main.ftl`；
- theme 来自 `peri-theme` atoms，不硬编码组件颜色；
- terminal control sequence 在进入 renderer 前剥离；
- 用户可关闭 mouse capture 时，键盘路径仍完整。

## 19. 事件与规范对齐

### 19.1 必须修订 `TUI-EVENT-001`

**当前基线**：现有标准“消息区只消费鼠标滚轮”已与代码事实不一致（消息区已消费点击、拖拽、scrollbar 与键位）。

**目标规范**：实现本设计时，应把规则修订为以下语义，并同步 `peri-tui/CLAUDE.md`：

当前标准“消息区只消费鼠标滚轮”已与代码事实不一致。实现本设计时，应把规则修订为以下语义，并同步 `peri-tui/CLAUDE.md`：

> 键盘事件按当前 `FocusOwner` 分发；鼠标事件按 z-order、坐标与 semantic hit region owner 分发。消息区可以消费其注册区域内的 wheel、scrollbar/text-selection gesture、显式 control、entry header 事件，以及消息区获得 focus 后声明的键位；不得消费区域外、无语义目标或属于 composer/前景层的事件。Modal/popup/completion/panel/Inspector 的局部取消、滚动和点击优先于背景。所有鼠标语义动作必须有键盘等价。

建议在稳定标准中另增：

- `TUI-POINTER-001`：HitMap、Down+Up、drag threshold、hover/focus、stale geometry；
- `TUI-SCROLL-001`：ScrollRegion ownership、无 scroll chaining、follow/browse 与 overlay 行为。

本文只定义目标；这些稳定规则应在实现阶段进入 `docs/standards/tui.md`，不能仅靠 design 文档覆盖旧标准。

### 19.2 事件链要求

新增或修改以下数据时必须覆盖完整 `ARC-EVENT-001` 链路：

- requested outer、canonical outer、delegated requested target、effective target、wrapper、descriptor/origin；
- normalized effective input 与 approval 后实际 input；
- AwaitingApproval/Denied/Cancelled/TimedOut 等 lifecycle；
- detail availability/revision/opaque ref；
- `ToolOutputChunk`/progress；
- structured diff/log/resource facets；
- history replay 与 terminal event。

终止事件必须让 TUI 离开 loading。SubAgent/Workflow 来源轴已实现：`_peri.sourceAgentId` 无条件注入 mapped tool 事件并解码为 `agent_id`（见 §8.2）；剩余协议依赖是稳定 `turn_id/generation`、`sequence` 与 canonical/effective identity。不得恢复 Agent→TUI 第二套直连事件通路。

### 19.3 协议兼容

- 标准 ACP `rawInput/rawOutput` 继续可作为 legacy projection，但新 `_peri` metadata/caps 必须按 session capability 门控；
- 老客户端忽略扩展字段仍能显示基本 tool call；
- 老 session 没有 effective identity/detail ref 时走 safe Generic + Legacy unavailable；
- replay 与 live path 对同一工具产生等价 ToolEntryVm；
- 不因新 Inspector 将敏感 raw payload 无条件广播给所有客户端。


## 20. 验收标准

**目标规范**：以下验收条件是 redesign 完成后必须满足的可验证标准。

### 21.1 信息架构

- **IA-001**：每个 tool invocation 始终有且只有一个 Inline audit entry。
- **IA-002**：Inline 不出现 raw JSON、完整 stdout、文件正文、skill body 或 workflow script。
- **IA-003**：Expanded block 不拥有独立 scrollbar。
- **IA-004**：普通 tool detail 进入 Inspector，不进入 decision Modal。
- **IA-005**：被 grouping 隐藏的每个调用仍可逐项 focus、copy、展开和查看详情。
- **IA-006**：Inspector 打开/关闭不修改 transcript fold/follow。
- **IA-007**：详情不可用或截断时，UI 明确显示 completeness。

### 21.2 工具覆盖

- **TOOL-001**：§4.1 的每个静态工具都有明确 presenter/family。
- **TOOL-002**：有 verified identity snapshot 时，`ExecuteExtraTool` 以 effective tool 为主显示、以 wrapper 为 provenance；legacy raw wrapper 明确显示 requested target，不冒充 resolved target。
- **TOOL-003**：`mcp__{sanitized_server}__{sanitized_tool}` 稳定显示安全 friendly label；原始 server/tool provenance 在 snapshot 可用时保真展示，否则明确为 legacy-derived。
- **TOOL-004**：未知工具至少显示 friendly title、状态、参数数量和 detail availability。
- **TOOL-005**：`folder_operations` 按 operation 区分 verb/effect。
- **TOOL-006**：`AgentResult` 优先关联原后台 Agent，无法关联时明确 synthetic。
- **TOOL-007**：Workflow 的 `Launched` 与最终 `Completed/Failed` 可区分。
- **TOOL-008**：artifact 明确显示 public upload 与 expiry。
- **TOOL-009**：legacy `Skill`、大小写无关 alias `reading`/`Shell`/`task`/`WriteSandbox`（含历史 `Task` spelling）进入 canonical presenter。
- **TOOL-010**：新工具未加入 exact registry 时仍通过 Generic fallback，不 panic、不空白。

### 21.3 Pointer 与键盘

- **INPUT-001**：entry header click 与 focused `Enter` 结果一致。
- **INPUT-002**：action 只有同 target、同 frame 的 Down+Up 才执行。
- **INPUT-003**：Down 后位移超过 2 列或 1 行，即使缺少 Drag 事件也不得 click。
- **INPUT-004**：正文 drag 只进行文本选择，不展开 entry、不激活 action。
- **INPUT-005**：hover 不移动 focus、不改变 fold、不发送网络请求。
- **INPUT-006**：resize、rewind、session reset 后旧 HitMap 不触发动作。
- **INPUT-007**：Modal/Panel/Inspector 关闭的 release 不 click-through。
- **INPUT-008**：所有 hover action 有键盘路径；无 Moved 事件功能完整。
- **INPUT-009**：keyboard 与 mouse 调用同一业务 `UiCommand` 或等价单一处理函数。
- **INPUT-010**：Composer editing 时 transcript 单字母命令不截获文本。

### 21.4 滚动与 follow

- **SCROLL-001**：wheel 只滚指针下最高 z-order ScrollRegion。
- **SCROLL-002**：Wide dock 下 transcript 与 Inspector 独立滚动。
- **SCROLL-003**：任一 region 到边界后 wheel 不链到背景。
- **SCROLL-004**：Modal 范围与遮罩内 wheel 不滚背景。
- **SCROLL-005**：BrowseHistory 中 streaming 不改变 transcript viewport。
- **SCROLL-006**：fold/group/resize 后操作 entry header anchor 稳定。
- **SCROLL-007**：Inspector 离底后新 output 显示独立 indicator。
- **SCROLL-008**：pending interaction 到达 BrowseHistory 时只显示 sticky indicator，不抢 viewport。
- **SCROLL-009**：高频 wheel event 被合并，停手 pending 最终 flush，反向滚动不回弹。
- **SCROLL-010**：打开/关闭 Inspector 不改变 transcript follow。

### 21.5 Streaming、错误与长输出

- **DATA-001**：同一 ToolKey 的所有更新只修改一个 entry。
- **DATA-002**：缺少 output chunk 协议时，running UI 不伪造 tail。
- **DATA-003**：Succeeded、Failed、Denied、Cancelled、TimedOut 可区分。
- **DATA-004**：错误始终保留一条非空、安全摘要。
- **DATA-005**：JSON object/array output 不因 `.as_str()` 失败而变为空。
- **DATA-006**：上游截断时显示 omitted count（若可知）与 detail availability。
- **DATA-007**：100k 行 detail 的 render/hover/wheel 不与总行数线性增长。
- **DATA-008**：迟到 chunk/response 不覆盖新 selection，不将终态复活为 Running。
- **DATA-009**：live 与 replay 对同一调用产生等价 identity/lifecycle/summary。
- **DATA-010**：detail cache 按 session 隔离，并在 lifecycle 边界正确清理。

### 21.6 Approval 与安全

- **SAFE-001**：点击 approval Modal 正文、边框、空白和遮罩均不批准。
- **SAFE-002**：批准不在 MouseDown 发生，默认 focus 是最小权限 action。
- **SAFE-003**：双击、重复 Enter 或重复响应只发送一次 decision。
- **SAFE-004**：synthetic secret marker 不出现在 Inline、Expanded、Inspector、Modal、copy、debug ViewModel 或 telemetry。
- **SAFE-005**：URL userinfo、敏感 query/header 与 command secret 被脱敏。
- **SAFE-006**：unknown tool 不使用任意首个 JSON 字段作为摘要。
- **SAFE-007**：审批展示 effective tool、effective args、origin 与 batch scope。
- **SAFE-008**：详情 RPC 只接受 session-bound opaque ref/ToolKey，不能读取任意路径。
- **SAFE-009**：MCP/plugin output 的 ANSI/OSC/control sequence 不进入 terminal renderer。
- **SAFE-010**：UI 不提供 Reveal secret、历史 rerun/undo 或直接 approve context action。
- **SAFE-011**：approval result 写回 durable transcript，replay 后可辨认。
- **SAFE-012**：Agent delegation 明确展示子 Agent HITL trust boundary。

### 21.7 响应式与 accessibility

- **RESP-001**：覆盖 120×30、100×24、80×24、48×16、40×8、30×8。
- **RESP-002**：Narrow 下 action 纵向排列且全部可达。
- **RESP-003**：低高度下 Modal/Inspector 不越界，transcript 普通 workspace 至少 3 行。
- **RESP-004**：Inspector resize 跨 dock/drawer/page 后保留 selection/facet/scroll。
- **A11Y-001**：NO_COLOR 下所有 lifecycle、risk 与 focus 可辨认。
- **A11Y-002**：ASCII fallback 下状态不丢失。
- **A11Y-003**：CJK、emoji、combining mark 的 hit/truncate/selection/copy 一致。
- **A11Y-004**：Reduced motion 下无持续高显著动画，仍显示 elapsed/status。
- **A11Y-005**：KeyboardOnly 可完成 fold、details、facet、scroll、copy、approval、AskUser、close。
- **A11Y-006**：clipboard 失败有可见反馈，不静默丢失用户动作。

### 21.8 规范与性能

- **SPEC-001**：修订后的 `TUI-EVENT-001` 与代码实际消费事件一致。
- **SPEC-002**：`peri-tui/CLAUDE.md` 不再声称 MessageArea 只能消费滚轮。
- **SPEC-003**：新增 tool/detail/output 事件完整满足 `ARC-EVENT-001`。
- **SPEC-004**：TUI 详情请求经 ACP transport，不直接驱动 Agent/middleware。
- **SPEC-005**：render body 不写业务 atom，hook 顺序稳定。
- **SPEC-006**：新增用户文本存在于两份 locale，颜色来自 theme。
- **PERF-001**：只为 viewport 生成 HitRegions。
- **PERF-002**：同一 hover target 内 Moved 不触发 redraw。
- **PERF-003**：Inspector 关闭时不解析完整大 output/diff。
- **PERF-004**：idle 时无 hover/spinner/detail 导致的高频 redraw。

## 21. 验证场景与命令

**目标规范**：以下验证场景用于验收；当前已具备的能力以 §3.2 为准，未实现的场景是目标路径。

### 22.1 Golden scenes

至少提供：

- Read range success；Glob/Grep 大结果截断；
- Edit diff success/conflict；Write create；SandboxWrite scope；
- Bash running/completed/error/background/timed out；
- WebSearch/WebFetch/artifact public upload；
- Skill load/discovery；Todo/Goal；
- Agent sync/background/fork/resume + AgentResult；
- Workflow launched/running/final failed；
- Cron register/list/remove；LSP diagnostics/references；
- MCP resource、known MCP tool、unknown dynamic tool、malformed ExecuteExtraTool；
- HITL single/batch/stale/response failure；AskUser single/multi/custom；
- legacy replay without descriptor/detail；
- NO_COLOR/ASCII/Reduced/KeyboardOnly；
- CJK/emoji/combining path、query、selection。

### 22.2 Interaction scenarios

- hover header → click disclosure → drag body → scroll thumb → open Details → close/restore；
- Inspector dock 中分别滚 transcript 与 detail；
- BrowseHistory 收到 streaming、approval 与后台 completion；
- Modal body scroll 到边界，验证背景不动；
- MouseDown action 后移出、resize、session switch、再 MouseUp；
- 高频 wheel burst、停手 flush、快速反向；
- resize 跨 Wide/Standard/Narrow；
- clipboard unavailable；
- terminal 不发送 Moved/Drag；
- stale detail response 到达已切换 session。

### 22.3 目标命令

文档变更阶段：

```bash
git diff --check
```

TUI 实现阶段至少运行：

```bash
cargo check -p peri-tui
cargo test -p peri-tui --lib
cargo clippy -p peri-tui --all-targets -- -D warnings
```

涉及事件/ACP 扩展时，额外运行相关 mapper、event-chain 与 replay tests；跨 workspace 收口时按仓库标准运行 workspace clippy/test。涉及鼠标、PTY、resize、面板和弹窗时，补充 E2E 场景，命令以 `e2e/CLAUDE.md`/测试规范为事实源。

## 22. 设计完成定义

**目标规范**：

只有同时满足以下条件，才可称为本设计完成：

1. 当前静态工具、dynamic MCP/plugin 与未来 unknown tool 均有安全、友好的 Inline UI；
2. 有 verified resolver identity 时，`ExecuteExtraTool` 不再遮蔽 effective action；legacy wrapper 明确区分 requested 与 resolved；
3. 长 output/diff/log/SubAgent/Workflow 进入响应式非模态 Inspector；
4. Modal 只承担决策，审批不存在整窗/MouseDown 误批准；
5. mouse 使用稳定 semantic HitMap，keyboard/mouse 等价；
6. transcript、Inspector、Panel、Modal 的滚动所有权清晰且不串滚；
7. BrowseHistory、resize、streaming 与 detail follow 不抢 viewport；
8. 敏感内容在 wire/持久 ViewModel 前脱敏，copy/hover/error 不绕过；
9. Narrow、低高度、NO_COLOR、ASCII、Reduced、KeyboardOnly 可用；
10. 标准、模块指引、事件契约、测试与实现同步，无平行事实源。

## 23. 事实源索引

| 主题 | 稳定事实源 |
| --- | --- |
| TUI 规则 | `docs/standards/tui.md` |
| 跨层事件/边界 | `docs/standards/architecture-contracts.md` |
| TUI 模块路由 | `peri-tui/CLAUDE.md` |
| BaseTool / descriptor | `peri-acp-types/src/tools.rs` |
| 工具生产链 | `peri-agent/src/session/factory.rs::production_blueprint`、`peri-middlewares/src/assembly.rs` |
| Tool event 发射 | `peri-agent/src/agent/stages/tool_dispatch.rs`、`peri-acp-types/src/event_v2.rs` |
| ACP 映射 | `peri-acp/src/event/mapper.rs`、`peri-acp/src/session/event_sink.rs` |
| TUI 通知/bridge | `peri-tui/src/kit/acp_notifier.rs`、`peri-tui/src/kit/acp_bridge.rs`、`peri-tui/src/kit/acp_events/` |
| 后台完成路由 | `peri-agent/src/session/async_router.rs`、`peri-agent/src/agent/executor.rs`（`on_bg_complete`）、`peri-agent/src/agent/continuation.rs` |
| Agent 装配路径 | `peri-agent/src/session/exec/stage_builder.rs`、`peri-agent/src/session/subagent.rs`、`peri-middlewares/src/subagent/`、`peri-agent/src/agent/workflow/agent.rs` |
| Tool ViewModel/语义 | `peri-tui/src/kit/tui_render_unit.rs`、`tool_semantics.rs`、`truncate.rs` |
| Transcript/render/input | `peri-tui/src/kit/message_area/`、`focus_router.rs`、`mouse_router.rs` |
| Layout/Panel/Popup | `peri-tui/src/kit/layout.rs`、`panel_overlay.rs`、`popup_overlay.rs`、`panels/`、`popups/` |
| HITL policy | `peri-middlewares/src/hitl/mod.rs` |
| 动态工具 | `peri-middlewares/src/tool_search/`、`mcp/`、`skills/`、`subagent/`、`cron/`、`lsp/`、`goal/`、`peri-workflow/src/tool.rs` |
| Active chat redesign decisions | `spec/issues/2026-08-10-chat-redesign-slice2-onwards.md` |
| Scroll active issue | `spec/issues/2026-08-04-scroll-wheel-mapping-disharmony.md` |

本文不复制工具注册数量作为永久工程规则；新增/移除工具时，以 `BaseTool` 实现、生产链装配与契约测试为事实源，并同步 §4/§12 的展示覆盖矩阵。
