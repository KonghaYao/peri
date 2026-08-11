# Perihelion TUI Chat Message Flow — Redesign Style

> 状态：提案（greenfield）  
> 范围：`peri-tui` 的 chat transcript、消息状态、滚动与 composer 邻接区域  
> 视觉参照：`../grok-build` 的 scrollback block system  
> 非目标：复刻 Grok 品牌、兼容现有 `TUI-STYLE.md`、规定 ACP 数据协议

## 1. 文档定位

本文是 chat 消息流 redesign 的独立视觉与交互规格。它不继承当前 Perihelion TUI 的气泡、卡片、颜色或间距约定；实现阶段应以本文定义的体验为目标，再把现有 `TuiRenderUnit` 映射到新视觉系统。

参考 Grok 的不是具体品牌，而是四个结构性决策：

1. transcript 是连续的工作日志，不是左右对话气泡墙；
2. 每个 entry 共享稳定的水平网格，用一列 accent 表达类型与状态；
3. reasoning、tool、subagent 等过程信息默认压缩，主回答保持最高阅读优先级；
4. running、selected、error 等状态通过多重线索表达，不只依赖颜色。

## 2. 设计目标与原则

任何时刻，用户都应能一眼判断：我刚刚要求了什么、Agent 正在做什么、哪些动作成功/失败/待确认，以及最终回答从哪里开始。

- **内容优先，chrome 后退**：正文、命令、路径、diff 是第一层；禁止给每条消息画完整矩形边框。
- **一条时间轴，一套网格**：所有类型进入同一条左对齐时间轴，不使用左右气泡分栏。
- **过程可见，但默认安静**：运行过程可见，完成过程主动收束；失败和交互不得自动隐藏。
- **状态变化不引发布局跳跃**：streaming 只向下增长，spinner、label、duration 不造成前缀抖动。
- **键盘优先，鼠标等价**：hover 操作必须有 focus/selection 等价态。
- **语义 token**：组件请求 `accent.user`、`status.error` 等角色，不写私有颜色。

## 3. 页面结构

```text
┌──────────────────────────────────────────────────────────────┐
│ optional session header / context notice                    │
├──────────────────────────────────────────────────────────────┤
│  transcript viewport                                         │
│  │  user prompt                                              │
│  │  thinking / tool activity                                 │
│  │  assistant response                                      ▐│
│  │  system event                                             ▐│
├──────────────────────────────────────────────────────────────┤
│ transient status: mode · queue · follow state                │
│ composer                                                     │
│ key hints / model / context                                  │
└──────────────────────────────────────────────────────────────┘
```

区域优先级：composer 完整可操作；当前 turn 的活动 entry 尽量可见；transcript 获取剩余高度；header 和 key hints 在窄屏降级。

### 3.1 Transcript 水平网格

```text
outer  accent  gap   content                          gap  scroll
  1      1      2      flexible                        1      1
         │
```

- `outer`：selection border 或安全区，默认 1 cell。
- `accent`：固定 1 cell，贯穿 expanded block；collapsed entry 可退化为 bullet。
- `gap`：默认 2 cells；宽度小于 60 时缩为 1。
- `content`：所有消息共享左起点。
- 滚动条只在内容溢出或用户滚动时出现。
- 最大可读宽度建议 100 cells；更宽时余量留在右侧。

禁止用户消息右对齐、按内容宽度生成气泡、不同 entry 使用不同正文起点。

### 3.2 垂直节奏

- 新 user prompt 前保留 1 个空行，定义 turn 节拍。
- 同一 turn 内 entry 默认无空行，以 accent、bullet、label 分段。
- assistant 最终回答与前一过程 entry 之间允许 1 个空行。
- expanded diff/code block 自带内部 padding，不叠加 entry gap。

## 4. 视觉 token

默认主题采用 Tokyo Night 方向，但 token 必须保持语义化：

| Token | 默认值 | 用途 |
| --- | --- | --- |
| `surface.base` | `#24283B` | transcript 主背景 |
| `surface.raised` | `#292E42` | composer、expanded tool body |
| `surface.sunken` | `#1A1B26` | code、terminal output |
| `surface.selection` | `#283457` | 键盘选择背景 |
| `text.primary` | `#C0CAF5` | 正文、当前选择标题 |
| `text.secondary` | `#A9B1D6` | 次级正文 |
| `text.muted` | `#737AA2` | tool 摘要、metadata |
| `text.dim` | `#565F89` | 标点、时间、折叠提示 |
| `accent.user` | `#7AA2F7` | 用户 prompt |
| `accent.assistant` | `#BB9AF7` | assistant 回答 |
| `accent.reasoning` | `#545C7E` | reasoning |
| `accent.tool` | `#737AA2` | 完成的 tool |
| `status.running` | `#7DCFFF` | 活动状态 |
| `status.success` | `#9ECE6A` | 成功完成 |
| `status.warning` | `#E0AF68` | 警告、待确认 |
| `status.error` | `#F7768E` | 错误、失败 |
| `syntax.command` | `#E0AF68` | shell command |
| `syntax.path` | `#FF9E64` | 文件路径 |

这些 hex 是默认主题值，不得写入具体组件。

### 4.1 文本与符号层级

- Primary：普通正文，不默认 bold。
- Label：类型名称或动作动词，bold；每行最多一个 bold 主锚点。
- Secondary：结果摘要、duration、计数。
- Dim：折叠提示、符号、辅助标点。
- Reasoning body：muted + italic；无 italic 时用 dim。
- Error body：tool 错误摘要块整块 error 色（§9.2 拆行口径）；正文长输出仍以可读性优先。

| 语义 | 符号 | 文本后备 |
| --- | --- | --- |
| running | `◐` | `Running` |
| success | `✓` | `Done` |
| error | `×` | `Failed` |
| warning / approval | `!` | `Needs approval` |
| collapsed / expanded | `▸` / `▾` | `collapsed` / `expanded` |
| queued | `·` | `Queued` |

> 无 user prompt 符号/文本后备：role label 行（`› You`）已移除（§6.1），用户正文在空行后直接开始。

Unicode 能力不足时分别退化到 `*`、`+`、`x`、`!`、`>`、`v`、`.`。任何状态都不能只依赖颜色。

## 5. Entry anatomy

```text
│  [bullet] Label  primary summary              metadata
│           optional secondary line
│           expanded body
│           optional action hint
```

Label 只陈述类型或动作；summary 回答“做什么”，不能 dump 原始 JSON；metadata 回答“状态怎样”；hint 只在 selected/focused 时显示。

## 6. 消息类型规格

### 6.1 User prompt

```text
│  重新设计整个 chat 消息流，参考 grok-build。
```

- 左对齐，不使用气泡背景；accent 为 `accent.user`。
- 无 role label 行：正文在 turn 空行（§3.2）后直接开始；`You` 不再渲染，
  `msg-user-prompt` locale key 保留但已无消费者。
- 保留用户换行；长 prompt 默认最多 6 个视觉行，之后显示 `… +N lines`。
- slash command、skill token、`@mention` 只做局部强调。
- channel、cron、system-reminder 按 system event 渲染并显示来源，不伪装为用户。

### 6.2 Assistant response

```text
│  我建议把消息流改成统一的左侧时间轴……
```

- assistant 正文是 transcript 中视觉权重最高的长文本。
- 无 role label 行：`Perihelion` 不再渲染，正文用 `text.primary`；
  `accents.assistant` palette 保留但已无消费者（reasoning 折叠/展开符号使用 dim）。
- 同一 streaming response 只能有一个 entry，不按 chunk 新增 block。
- Markdown heading 主要依靠 bold 与空行，不使用高饱和彩虹色。
- 所有 md 行（段落/标题/列表项/代码行）在 convert 阶段按 content 宽度折行
  （grapheme + display width，§12 口径，超宽单词切分不丢内容）——渲染层只在
  视口宽度 wrap，convert 阶段不折行会导致二次折行、折出行丢失 `│` 竖线前缀。
- code block 使用 `surface.sunken`；复制时不包含边框、语言标签或行号。
- 完成后可在末行显示 `12.4s · 1.8k tokens`，默认不独占一行。

### 6.3 Reasoning / thinking

```text
   ◐ Thinking…                                      8s
     正在检查消息类型和渲染入口……

   ▸ Thought for 12s                                14 lines
```

- running 显示 label + 最近 2–4 个视觉行的 tail preview。
- completed 自动折叠为单行，保留 duration 与行数。
- 展开正文使用 dim/italic，始终低于 assistant 正文权重。
- 空 reasoning 仍显示 `Thinking…`，不能出现空白 block。
- 用户手动展开或滚离底部后，不得强制重新折叠或抢回 viewport。
- 隐藏 reasoning 只影响 body；活动状态行仍需可见。

### 6.4 Tool activity

```text
   ✓ Read  peri-tui/src/kit/message_area/render.rs   37ms
   ◐ Bash  cargo test -p peri-tui                   4s
   × Edit  render.rs                                conflict

│  ▾ Bash                                            4.2s
│    $ cargo test -p peri-tui
│    ───────────────────────────────────────────────────
│    test result: ok. 895 passed
```

- tool 默认是 compact activity row，不是重型 card。
- label 使用面向人的动词；原始 tool name 仅作后备。
- 摘要优先展示 path、query、command、URL host 等主要对象。
- success 默认折叠；running 最多展示 3 行 tail；error 自动展开错误摘要。
- expanded body 才显示完整输出；collapsed row 禁止打印 JSON arguments。
- command 使用 `syntax.command`，path 使用 `syntax.path`。
- error 首行使用 error 符号 + error 色 bold 错误词（`— Failed`）；错误详情按
  ` - Error: ` 分隔符拆为独立行，整块 error 色（§9.2），不只靠单行文字表达失败。
- duration 仅在宽度充足时右对齐；窄屏紧跟 summary。

专属展示：Read/Glob/Grep 显示目标和结果数；Bash 显示 command、stdout/stderr、exit code；Edit/Write 显示文件、`+N −M` 和 diff；TodoWrite 显示进度；Skill 显示名称；AskUserQuestion/approval 升级为 interaction block。Edit/Write 的 completed header 只保留 diff 计数后缀（`· +N · −M`），不再拼接重复的输出摘要文本（摘要中的路径与 header 的 `input_summary` 重复）。

### 6.5 Diff

```text
│    src/render.rs  +3 −1
│    42   context
│    43 - old line
│    43 + new line
```

- 默认展示首个 hunk 与最多 8 个 change 行，其余显示 `… +N more lines`。
- insert/delete 同时使用 `+`/`-`、foreground 与低对比背景，不能只靠红绿。
- 行号 gutter 使用 dim，不参与正文复制。
- 窄屏先隐藏旧行号，再裁切代码；代码默认不软换行。

### 6.6 System event

```text
   ── Context compacted · 18k → 7k ─────────────────────
   !  Model switched to claude-sonnet-4-5
   ×  Connection lost · retrying in 3s
```

普通事件使用单行 divider；warning/error 使用 accent 和明确 label；来源必须可辨识。compact、model switch、session restore 不得伪装为 assistant。错误应显示恢复动作或下一步操作。

### 6.7 Subagent / background task

```text
   ◐ Agent explorer  Inspecting message flow          6 tools
   ✓ Agent explorer  Found 8 UI patterns              12s
```

- 默认单行，显示名称、activity/结果摘要、tool count。
- running 摘要可以更新，但前缀宽度稳定。
- Enter 打开 nested transcript 或详情 pane，不把完整嵌套消息铺入主时间轴。
- failed 自动显示错误原因；多个并行 agent 各占一行。

### 6.8 Interaction block

```text
│  ! Approval required
│    Bash wants to run: cargo test --workspace
│
│    [Allow once]  [Always allow]  [Deny]
```

- permission、AskUserQuestion、plan approval 必须 expanded、可聚焦并进入 tab order。
- 当前选项使用 selection background + border + bold。
- 不得被新 streaming chunk 滚出视口；等待时 follow mode 锚定此 block。
- 提交后转为只读结果行，如 `✓ Allowed once`。

### 6.9 Todo / progress

当前 in-progress 项可在活动 turn 底部展示；completed 使用 muted + `✓`；摘要格式为 `3/7 tasks · Running tests`；最终 assistant 回答不得被 todo footer 隔断。

## 7. 折叠与分组

统一支持 `Collapsed`（单行）、`Preview`（有界 tail）与 `Expanded`（完整 body）：

| Entry | Running | Completed | Error |
| --- | --- | --- | --- |
| user | Expanded | Expanded / 长文折叠 | — |
| assistant | Expanded | Expanded | Expanded |
| reasoning | Preview | Collapsed | Preview |
| tool | Preview | Collapsed | Expanded summary |
| subagent | Collapsed + live summary | Collapsed | Expanded summary |
| system | Collapsed | Collapsed | Expanded summary |
| interaction | Expanded | Collapsed result | Expanded |

用户手动改变 fold state 后，本 turn 内不再被自动策略覆盖。running 变 completed 时，仅未被手动操作的 entry 可自动折叠。`Enter` 切换 collapsed/expanded；`Space` 可切换 preview。group 必须显示隐藏数与失败数。

相邻、成功、低信息密度 tools 可压成 `▸ Inspected 8 files · 3 searches`。不得合并 running、error、interaction、含 diff 的 edit、当前 selected entry，也不能跨越 assistant 正文或 system event。

## 8. Streaming 与滚动

### 8.1 Follow mode

- 初始为 `FollowBottom`。
- 用户向上滚动超过 1 行即进入 `BrowseHistory`。
- `BrowseHistory` 中新内容不得移动 viewport；底部显示 `↓ New output`。
- 滚到底、点击 indicator 或按 `End` 恢复 follow。
- resize 后维持内容锚点，不简单重置到底部。

### 8.2 动画与 Markdown

- 同屏最多一个高显著 spinner；其他 running entry 使用低显著 pulse 或静态符号。
- animation tick 不触发 transcript 全量 reflow；reduced-motion 使用静态符号 + elapsed time。
- 未闭合 markdown 按普通文本稳定渲染，闭合后再升级样式。
- code fence streaming 时保持固定 gutter 与背景。
- assistant 与 reasoning 分成视觉独立 entry。

## 9. Selection、focus 与 copy

鼠标 hover 不改变 entry 背景；focused entry 使用 selection border；text selection 使用 `surface.selection`；interaction option 使用 selection background + bold。

复制语义内容而不是屏幕像素：不复制 accent、bullet、chevron、border；code 不复制语言标签和行号；diff 复制 patch 标记和正文；Read header 复制 path，Bash header 复制 command；Unicode 坐标按 terminal display width 计算。role label 行（`You` / `Perihelion`）已从渲染移除（§6.1/6.2），语义复制无需再剥离角色名——选中文本直接以正文开始；tool header 语义为 `{Verb} {summary}{suffix}`（Edit/Write 的 suffix 即计数，§6.4）。md 折行（§6.2）产生的软换行随复制保留（每行剥离 `│ ` gutter 后拼回）。

### 9.1 单击展开（鼠标等价 Enter）

- **命中区域**：entry 的逻辑首行（header/label 行）；正文行（含 wrap 续行）不响应，留给文本选区/复制。
- **单击判定**：`Down` + `Up` 且无 `Drag`（未拖拽）；`Down`/`Up` 坐标差 ≤1 行、≤2 列（手抖容差），超差视为拖拽意图。
- **语义与键盘 Enter 完全一致**：tool / reasoning / completed interaction 切换 Collapsed↔Expanded（写 `FOLD_OVERRIDES` + `user_modified`）；subagent 打开详情 pane；无折叠能力 entry（user / assistant 正文）仅获得焦点不动作；pending interaction 首行仅聚焦不提交（Enter 的提交是明确按键语义）。
- **焦点联动**：点击同时设置 entry 焦点（selection border）与 `FOCUSED_ENTRY_KEY`，Alt+Up/Down 可继续键盘导航；interaction option 重置到首项。
- **仲裁**：滚动条列、keepgoing / md 复制 / interaction option 热区优先（先注册 Consumed）；拖拽释放（`dragging`）放行给选区复制；点击同时清除残留选区。

### 9.2 Tool error 输出

```text
Tool execution failed: Read
- Error: File not found at /Users/konghayao/code/ai/perihelion/peri-tui/…
```

- error tool 的 `output_summary`（`Tool execution failed: {Verb} - Error: …`）按
  首个 ` - Error: ` 分隔符拆成两行：首行 `Tool execution failed: {Verb}`，
  次行 `- Error: {详情}`——错误详情不再与工具名挤在同一行。
- 拆行后的错误摘要块整块使用 `status.error` 色（error 状态恒展开显示）。
- 行数上限与普通 tool 输出一致（Standard 最多 4 行 / Compact 2 行），超出 `… +N more lines`。

## 10. Composer 与消息流

Composer 是唯一长期使用完整边框的区域：

```text
╭────────────────────────────────────────────────────── session ╮
│ 输入消息…                                                    │
╰─ @ files · / commands ───────────────────────────── 42% ctx ─╯
```

- active border 使用 focus accent，inactive 使用 dim border。
- mode/model 只在状态栏显示；composer 顶栏仅保留右侧 session title，context/hints 放 footer；窄屏逐级隐藏。
- 输入正文与 transcript content 起点尽量对齐。
- attachment 使用 inline chip，不能侵占超过 composer 可见高度一半。
- queued prompt 位于 composer 上方，不提前进入 transcript；发送后只出现一次。
- interjection 使用 user 样式并增加 muted metadata。
- `Esc` 随焦点只执行一个层级的取消。

## 11. 响应式降级

- **Wide `>= 100`**：content 最大 100 cells；metadata 可右对齐。
- **Standard `60–99`**：默认布局；metadata 紧跟 summary；隐藏常驻快捷键 hint。
- **Compact `40–59`**：accent gap 缩为 1；隐藏非关键 duration；tool summary 最多 2 行。
- **Narrow `< 40`**：accent 线退化为 bullet；无 metadata 列；interaction actions 垂直排列。

高度 `< 12` 时隐藏 session header 和 key hints；`< 8` 时 composer 限制为 1–2 行且 transcript 至少保留 3 行。modal/interaction 不得大于 viewport。

## 12. Accessibility 与兼容

语义至少组合 color + symbol/label；`NO_COLOR` 下保留 modifier、symbol 与明确状态文本；无 truecolor 时映射 ANSI 近似值；无 italic、undercurl、braille animation 时静默降级；截断按 grapheme/display width 处理并测试 CJK、emoji、combining mark；空闲时不持续高频重绘。

## 13. 当前 ViewModel 的建议映射

| 当前类型 | 新 entry |
| --- | --- |
| `TuiUserBubble` | User prompt / 来源型 System event |
| `TuiAssistantBubble.text` | Assistant response |
| `TuiAssistantBubble.reasoning` | 独立 Reasoning entry |
| `TuiToolCard` | Tool activity / Interaction block |
| `TuiSystemNote` | System event |
| `TuiSubAgentGroup` | Subagent row + detail view |
| `TuiCollapsedGroup` | Verb group |
| `TuiDivider` | Turn/system divider |
| `TuiAskUserBlock` | Interaction block |

视觉拆分 reasoning 不代表必须改变 ACP payload；renderer 可从一个 ViewModel 派生多个 visual sections。反之，tool input/output 即使来自多个事件，也必须保持一个稳定 entry。

## 14. 参考实现取舍

### 应吸收

共享 `accent + padding + content` 网格；semantic theme roles；thinking 三态；tool 的人类可读 summary；running/completed/error 的多重表达；离开底部后暂停 follow；selection chrome 不污染 copy；sticky turn 导航可作为后续增强。

### 不应照搬

Grok 品牌、文案、logo 与快捷键；过多 appearance knobs；每种 tool 都建立独立复杂 renderer；依赖 hover 才能发现核心操作；在主 transcript 内无限展开 nested agent transcript。

## 15. 验收标准

Golden scenes 必须覆盖：user + streaming assistant；reasoning 自动折叠；Read/Bash/Edit 三种状态；diff 的 120/80/48 columns；permission 与 AskUserQuestion focus；subagent 三种状态；system compact/model/reconnect；滚离底部后 streaming；queued prompt 不重复；`NO_COLOR`/ANSI/Unicode fallback；CJK/emoji selection；40×8 viewport。

行为断言：

- streaming chunk 不新增重复 assistant entry；
- completed 不改变用户手动 fold state；
- browse history 时新输出不移动 viewport；
- error 与 interaction 永不自动隐藏；
- copy 不含 UI chrome；
- `NO_COLOR` 下所有状态仍可辨认；
- resize 后 selection 与 scroll anchor 稳定；
- renderer 不在 render body 写业务状态；
- 每帧工作量与可见区域近似线性，而非与全历史长度线性增长。

## 16. 设计完成定义

主消息流不再依赖左右气泡；全部 entry 进入统一网格并使用 semantic token；reasoning/tool/subagent 使用一致折叠状态机；streaming、follow、selection、copy 有明确契约；每个 `TuiRenderUnit` variant 都有视觉归宿；wide、compact、narrow 与 `NO_COLOR` 均通过 golden scenes；用户无需阅读 tool JSON 也能理解 Agent 动作与结果。

## 17. Grok 参考依据

本文的核心取舍来自以下可验证入口，而不是对产品截图的猜测：

- 统一 scrollback 网格：`../grok-build/crates/codegen/xai-grok-pager/src/scrollback/layout.rs:9-23` 定义固定 1-cell accent、默认 2-cell left padding、flex content 与 right padding。
- 语义 theme：`../grok-build/crates/codegen/xai-grok-pager-render/src/theme/tokyonight.rs:48-105` 将背景、角色 accent、状态和 selection 定义成 semantic roles；默认 Tokyo Night 映射见同文件 `:156-216`。
- thinking 三态：`../grok-build/crates/codegen/xai-grok-pager/src/scrollback/blocks/thinking.rs:73-89` 定义 collapsed/truncated/expanded；运行结束折叠与用户切换逻辑见 `:428-512`。
- tool 摘要优先：`../grok-build/crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/execute.rs:164-220` 用 `$ command` 或人类动作 label 代替原始参数展示。
- 选择与 hover 分层：`../grok-build/crates/codegen/xai-grok-pager/src/scrollback/scrollback_pane.rs:204-261` 区分 hover background、selection background 和 border。
- sticky turn 与暂停 follow 的行为方向可从 `scrollback_pane.rs:351-440` 及 `tests/pty_e2e/wheel_scrolls_viewport_during_streaming_turn.rs` 验证。

这些路径只用于追溯设计来源。Perihelion 的实现仍应使用本仓库的数据流、theme atoms 与 `TuiRenderUnit`，不复制 Grok 内部类型。

