# TUI Chat 与 Tool Activity Workbench 目标设计

> 状态：已批准目标设计；实施进度见
> [`spec/issues/2026-08-10-chat-redesign-slice2-onwards.md`](../../spec/issues/2026-08-10-chat-redesign-slice2-onwards.md)
> 范围：`peri-tui` 的 transcript、tool activity、Inspector、阻塞交互、焦点、鼠标与滚动
> 边界：新增事件、详情请求或终态语义必须遵守 `ARC-EVENT-001` 与 `ARC-BOUNDARY-001`

本文只定义稳定产品契约。当前实现、入口与动态 inventory 以
[`peri-tui` 代码索引](../code-index/peri-tui.md)、源码和契约测试为准；未完成差距只写
active issue。

## 目标与原则

用户在任何时刻都应能判断：Agent 正在做什么、作用对象、调用状态、结果是否完整、
当前输入作用于哪个区域，以及内容是否被截断或脱敏。

- **正文优先，过程可审计**：最终回答保持最高阅读权重；每次调用保留稳定 entry。
- **连续时间轴**：user、reasoning、tool、assistant 与 system event 共用左对齐网格，
  不形成卡片墙。
- **默认安静，按需深入**：成功调用默认收束；错误、待决策与安全警告保持可见。
- **一条调用，一个身份**：同一调用的状态和输出原地更新，不按 event/chunk 追加副本。
- **安全摘要**：Inline 不 dump 任意 JSON、完整日志、prompt、skill body 或外部资源正文。
- **键鼠等价**：鼠标只加速已有命令；hover 不承载独占信息。
- **区域拥有事件**：键盘按 `FocusOwner`、指针按最新语义命中快照和 z-order 路由。
- **详情不阻塞**：普通详情进入 Inspector；Modal 只承载必须先响应的决策。
- **布局稳定**：spinner、duration、hover action 与流式更新不得移动 entry 主锚点。
- **状态不只靠颜色**：symbol、状态词和样式共同表达状态。

非目标：永久多栏 IDE、可拖动 pane、每张 entry 内独立 scrollbar、hover-only tooltip、
鼠标手势语义、点击历史调用重跑、在 TUI 绕过 ACP 读取或执行能力，以及为每个未来工具
编写专属组件。

## 信息披露与布局

每个 activity 使用四级披露：

1. **Inline**：永久审计行，格式为
   `{status} {verb} {primary object} · {result/risk} · {duration}`。
2. **Preview / Expanded**：在 transcript 内做有界核对；不得等同完整输出，也不拥有
   独立滚动条。
3. **Inspector**：显示协议可达的完整详情、分页内容或流式 tail；非模态。
4. **Modal**：只显示审批、OAuth、不可逆确认或复杂用户问题。

默认 fold：running 为 `Preview`，成功为 `Collapsed`；失败、拒绝、取消和超时显示明确
摘要，完整内容仍进 Inspector。用户手动 fold 后，生命周期更新不得覆盖其选择。
相邻、成功、低信息密度且 provenance 相同的调用可分组，但每个调用仍须独立可达；
待审批、运行中、失败、外部边界和 durable interaction 不得被隐藏进成功组。

布局规则：

- Wide 可将 Inspector dock 在 transcript 侧边，两者独立滚动；
- Standard/Compact 使用非模态 bottom drawer；Narrow 可使用全屏详情页；
- 非模态 Inspector 打开时 composer 保持可见；Modal 打开时背景完全 inert；
- transcript 延续 `GridSpec`、语义 theme token 与响应式断点；组件不得硬编码颜色；
- expanded preview 的视觉行数有界，超出时明确显示 `Open details` 与完整性状态。

Tool row 的 `verb` 使用可信 title/canonical name；`primary object` 回答“对什么做”，路径
优先 project-relative，URL 只显示脱敏后的 host/path；`result` 不重复对象。无安全摘要时
显示友好名称和参数数量，不猜测任意字段值。

## 工具身份与生命周期

目标 `ToolKey` 至少由 session、来源 owner/agent 和非空 `tool_call_id` 构成。slot、标题、
参数 hash 或 arrival order 不得作为唯一身份。调用还应保留四个不同概念：

- requested identity：模型请求的 outer name/input；
- canonical/policy identity：resolver 与审批使用的规范调用；
- effective identity：实际执行 target 与审批后参数；
- wrapper/origin：例如 `ExecuteExtraTool`、MCP server、plugin 或 builtin provenance。

只有协议提供可信 effective metadata 时，UI 才能把 wrapper 委托给 target presenter；否则
必须明确显示 `Requested … via …`，不得把请求名冒充已解析或已执行的 target。

生命周期、展示和详情是三个正交维度：

```text
Lifecycle: Queued → Resolving → AwaitingApproval → Running
           → Succeeded | Failed | Denied | Cancelled | Interrupted | TimedOut | NotExecuted
Presentation: Collapsed | Preview | Expanded
Detail: Closed | Open { surface, facet, revision, scroll, follow }
```

归约器必须满足：

- start/end、live/replay、重复事件和迟到事件均按 `ToolKey` 幂等收敛；
- end-before-start、缺失终态、空/重复 ID 等异常保留可见审计 entry，不静默丢弃；
- turn terminal 只补偿仍未终结的 entry，不把取消或中断伪装成成功；
- 迟到 chunk/end 不得复活已终结调用；历史 replay 不得覆盖 newer revision；
- live 与 replay 消费同一 finalized output 语义，后处理、截断与错误建议不得造成两套结果；
- parser、resolver 或审批前失败也必须获得可关联的 `NotExecuted`/失败记录。

## Presenter 与工具族

Presenter 管线固定为：

```text
可信 descriptor + effective identity + redacted input/output + lifecycle
  → family presenter
  → GenericSafe fallback
  → Inline / Preview / Inspector facets
```

| 工具族 | Inline 对象与结果 | Inspector 重点 |
| --- | --- | --- |
| `Read` / `Glob` / `Grep` / `folder_operations` | path、query、range、count | excerpt、matches、entries、完整性 |
| `Write` / `Edit` / `SandboxWrite` | path、scope、变更摘要 | structured effect、diff、sandbox roots |
| `Bash` | 脱敏 command、exit/background/timeout | stdout、stderr、exit、运行 tail |
| `WebFetch` / `WebSearch` | host/query、status/count | 结果 descriptor 与截断状态 |
| `artifact` | public upload、TTL | 已脱敏 URL、大小与公开边界 |
| `TodoWrite` / `goal` | 状态转移摘要 | revision、完整变更与证据 |
| `AskUserQuestion` / permission | pending 或回答摘要 | durable interaction；不作普通 Generic card |
| `SkillTool` / `DiscoverSkillsTool` | skill 名或发现条件 | source、descriptor；正文默认不展开 |
| `Agent` / `AgentResult` | 委派身份、模式与终态 | nested activity、thread/task provenance |
| `SearchExtraTools` / `ExecuteExtraTool` | discovery 或 requested target | wrapper、resolved/effective target、审批 provenance |
| Cron / `Workflow` | schedule/run 身份与状态 | phase、agent、log、result、可用操作 |
| `LSP` | operation、path/query、count | locations、diagnostics、server、完整性 |
| MCP resource / 动态 MCP tool | 外部 server/tool/resource 边界 | 调用时冻结 descriptor、content type 与结果 |
| 未知工具 | 友好名称、参数数量、明确状态 | 有界脱敏 metadata；不推断 effect 或安全性 |

通用 fallback 只从明确 allowlist 字段提取主对象；所有值先脱敏。malformed input/output
不得 panic。未知 effect 不显示 `safe`、`read-only` 等暗示，也不提供 rerun、undo 或审批
shortcut。

## Inspector 与详情安全

Inspector 是单实例 workspace surface，按所选 `ToolKey` 展示适用 facet，例如 Overview、
Input、Output、Diff、Log、Metadata、Timeline、Subagent 或 Workflow。切换 facet 保存各自
scroll/follow；切换调用时以 identity/revision 重新验证，不显示旧调用内容。

详情可来自 final event 的有界 snapshot，或带授权的 opaque `detail_ref`。若支持流式 output，
事件必须包含稳定 sequence、stream type、revision 与完整性；若支持详情请求，请求必须经
ACP、受 session/owner/tool/revision 约束，并可被取消。TUI 不自行从任意本地路径读取详情，
也不建立第二套持久化。

所有层级都必须：

- 显示 `complete / truncated / partial / unavailable`；未知省略量不得伪造计数；
- 应用同一 redaction policy，默认隐藏 token、password、cookie、authorization、私钥和
  连接串字段；
- 清理 ANSI 控制序列、OSC 8、双向控制符和终端 escape；外部 Markdown/HTML 按不可信文本
  处理；
- copy 再次执行脱敏和控制字符清理，不复制 gutter、fold marker 或视觉省略符；
- 大输出使用分页、line index 或 viewport virtualization，不复制进每个 ViewModel clone。

## 交互路由与审批安全

每个完成渲染帧产出只读语义命中快照：target identity、rects、z-index、scroll owner、
enabled state 与可执行 `UiCommand`。renderer 注册几何，router 执行业务命令；键盘和鼠标
最终生成同一命令。Modal、popover、Inspector/Panel、transcript action、scrollbar、entry
和 selectable body 按 z-order 命中，背景 shield 必须消费事件。

Pointer gesture 遵循 `Down → Pressed → Up/Cancel`：只有 Up 与 Down 命中同一 enabled
目标、同一 request/frame revision，且未发生 drag，才可激活。resize、scroll、session reset、
遮挡变化或 stale generation 取消 press。pointer capture 期间只有 owner 接收事件。

审批必须额外满足：

- 默认 focus 为最小权限动作，绝不默认批准；
- 点击正文、边框、空白、scrollbar 或遮罩不得批准；整窗点击批准路径必须移除；
- 提交后禁用 action，保证每个 request 最多一次响应；失败时保留交互并允许重试；
- 关闭 Modal 消费 release，禁止 click-through；
- `ExecuteExtraTool` 展示可信 effective target/args；`Agent` 明示子 Agent 的授权边界；
- 只展示协议实际支持的动作，不虚构 `Always allow`；permission mode 只展示后端事实。

`AskUserQuestion` 在 transcript 保留 pending/resolved 摘要。短单题可 inline；多题、多选、
自定义输入或长说明使用响应式 form。选项 click 只选择/聚焦，提交是独立动作。历史 replay
只恢复记录，不恢复已失效 request 的可提交能力。

## 焦点、滚动与连续性

`FocusOwner` 覆盖 composer、transcript、Inspector、Panel、Modal 和 popover。Modal 限制
focus；非模态 surface 关闭后恢复 opener；目标已消失时回到同一 surface 的最近语义邻居，
不得按旧 vector index 猜测。

可滚区域由各自 owner 管理：鼠标滚轮交给坐标下最高层 region，键盘滚动交给当前 focus。
region 到达边界后仍消费本次 wheel，禁止滚动穿透。滚轮不隐式改变 keyboard focus。

Transcript follow 状态只有 `FollowBottom` 与带稳定 entry anchor 的 `BrowseHistory`。浏览历史时
新内容只增加 unseen count，不移动 viewport；resize、fold、group 或 Inspector placement 变化
按 `EntryKey + intra-entry offset` 恢复锚点。pending decision、unseen failure、running activity
与 new output 可在 composer 上方形成有界 Activity strip，激活后跳到 entry/详情，但不得抢
viewport。

user prompt、assistant、reasoning 与 system event 延续连续 transcript：同一 message 不按 chunk
创建 entry；tool/subagent interleaving 仍形成语义段；reasoning 默认弱化并可折叠；来源明确的
system/background/cron/reminder 不得伪装成 user prompt。composer 在非模态浏览期间保持可用，
排队输入在 composer 附近展示而不提前伪装成已提交 transcript。

## 响应式与性能

- 宽度和高度断点由纯布局函数与语义布局状态决定，不在组件中散落 magic number；
- Compact/Narrow 依次隐藏装饰、duration 与次要 metadata，保留状态、对象、错误和安全动作；
- 低高度优先保留至少一行 transcript、composer 和阻塞 action；
- spinner、elapsed 与 hover 只更新可见 entry；Reduced/SSH profile 可关闭连续 hover；
- Inline/Expanded 成本与总输出规模无关；100k 行详情不得每帧全量 wrap/clone；
- publication scheduler、增量 Markdown 与 slot index 的性能契约见
  [TUI 流式 Markdown 性能设计](tui-streaming-markdown-performance.md)。

## 协议与兼容边界

目标依赖的新事实必须走完整事件链：Agent 发射、协议类型、ACP mapper/session、capability
门控（如适用）、TUI decoder/reducer 和测试。不能只在 TUI 从 title 或 payload 猜测。

协议应逐步提供：可信 requested/canonical/effective identity、terminal classification、
finalized structured output、completeness/detail ref、审批关联、stream revision，以及
SubAgent/Workflow 身份。历史或旧服务端缺字段时使用安全 fallback：保留 legacy requested
label、参数数量和明确 unavailable；不得伪造 canonical target、终态、详情或安全级别。

## 验收边界与事实源

目标实现至少验证：每个工具族和 unknown fallback；live/replay/重复/乱序 settlement；
Inspector 加载、取消和 stale response；审批 click safety 与键鼠等价；焦点恢复、区域滚动和
follow；窄屏、低高度、SSH/tmux、Unicode 与大输出；redaction、control-sequence 清理和
copy；新增协议的 caps 与旧服务端回放。

稳定路由：

- 当前 ACP → TUI 数据流：[tui-acp-data-flow.md](tui-acp-data-flow.md)
- SubAgent 展示：[tui-subagent-activity.md](tui-subagent-activity.md)
- TUI 实现入口：[peri-tui 代码索引](../code-index/peri-tui.md)
- 工具与 middleware 入口：[peri-middlewares 代码索引](../code-index/peri-middlewares.md)
- 跨层约束：[architecture-contracts.md](../standards/architecture-contracts.md)
- TUI 实现规则：[tui.md](../standards/tui.md)
