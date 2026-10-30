# TUI SubAgent 活动行设计

> 状态：现行设计
>
> 上位设计：[TUI Chat 与 Tool Activity Workbench](tui-chat-workbench.md)。如有冲突，
> 以上位设计为准。

实现位置：`peri-tui/src/kit/message_area/render/group.rs`（`render_subagent_group_lines`、
`subagent_tool_line`、`subagent_error_reason_lines`）；过程区续行前缀
`process_cont_prefix` 在 `render/helpers.rs`。

## 1. 目标

subagent 工具调用是**嵌套过程细节**，不是 transcript 顶层的独立事件。若与主时间线工具同口径，
会失去层级、让子任务每行与主任务同等醒目，并在并行 subagent 时放大噪声。

目标：subagent 工具行获得**明确区别于主时间线工具**的展示形态，体现「从属、弱化、可辨状态」，
且不改变交互契约（折叠/点击/复制）、不破坏网格与符号体系、通过 NO_COLOR 与窄屏降级。

## 2. 设计原则

| # | 原则 | 含义 |
| --- | --- | --- |
| P1 | 从属可视 | 工具行结构上必带「续行竖线 + 缩进」，与 subagent 块同属一列层级；正文起点与顶层 entry 不同 |
| P2 | 权重弱化 | label 去 bold；状态符号 dim 化——「bold + 状态色」是主时间线工具的专属锚点 |
| P3 | 错误不弱化 | error 是例外：符号与错误词升级到与主时间线同等显著（错误信息绝不因嵌套而看不清） |
| P4 | 动态一致 | running 符号仍用 braille 动画帧；低显著通过**颜色**而非动画有无表达 |
| P5 | 交互不变 | 工具行不可独立折叠、点击命中仍为 subagent 顶层行、语义复制仍输出 `{Verb} {summary}` |
| P6 | 锚点守恒 | 每屏每行最多一个 bold 主锚点：subagent 名（顶层行）与主时间线工具 label 是 bold；嵌套工具行不是 |

P2 与 P4 共同落实「同屏最多一个高显著 spinner」：高显著 = `status.running` 色动画帧（主时间线），
低显著 = `text.dim` 色动画帧（subagent 工具行）——两者都动，但显著度分层。

## 3. 形态规格（Wide / Standard）

```text
   ◐  Agent explorer  Inspecting message flow         6 tools   ← subagent 顶层行
   │  ⠋ Read  src/main.rs                             4s        ← 嵌套工具行（新形态）
   │  ✓ Bash  cargo test -p peri-tui                  0.4s
   │  × Edit  render.rs — Failed
   │  - Error: File not found at src/render.rs                  ← 失败原因行（与工具行同列）
```

逐列结构（Span 序列）：

| 段 | 内容 | 样式 |
| --- | --- | --- |
| ① 过程区续行前缀 | `[outer 空][indent 留空][dim 竖线][gap]`（`process_cont_prefix`，与 subagent 块续行同列；`indent` = 过程区右移量，≥60 列 2 / <60 列 0） | `sem.text.dim` 竖线 |
| ② 缩进 | 无独立缩进常量——`indent` 由 ① 前缀统一承担，组内行与组头同列 | 无样式 |
| ③ 状态符号 | running=braille 帧 / success=`✓` / error=`×` | running/success → `sem.text.dim`；error → `sem.status.error` |
| ④ Verb | `format_tool_name` 本地化动词（Read/Bash/Shell/Edit…） | `sem.text.primary`，**无 bold** |
| ⑤ 摘要 | `input_summary` 截断（预算减去缩进与 duration 列） | `sem.text.muted` |
| ⑥ 错误词 | error 时 ` — Failed`（i18n `msg-status-failed`） | `sem.status.error` + bold |
| ⑦ duration | running 整数秒（`4s`）/ completed 冻结值（一位小数秒 `0.4s`，UI 不出现 `ms`）；四舍五入后即 `0.0s` 的（< 50ms）不渲染，行在 summary 处结束；右对齐到居中带右缘（`line_width`；窄终端即 `term_width - 1`） | `sem.text.dim` |

要点：

- **缩进固定 2 格而非对齐 subagent 摘要**——流式下最近 3 行持续轮换，固定缩进保证前缀列稳定；
  该 2 格由过程区前缀统一承担（组头与组内行同列），无独立常量。
- **不用 `├─`/`└─` 树状末梢字符**——最近 3 行窗口滚动时末梢字符会跳动闪烁，且与现有
  「竖线续行」体系冲突；统一竖线 + 缩进即可表达层级，且实现与 wrap_map/语义复制零摩擦。
- ②③④⑤⑥⑦ 共用 `first_prefix` 之外的构造路径：全部行（含首行）走过程区续行前缀
  （`process_cont_prefix`），使 subagent 工具行**永远不是独立 entry 首行**——结构上即从属。

## 4. 与主时间线工具对照

| 维度 | 主时间线工具 | subagent 工具行（本设计） |
| --- | --- | --- |
| 正文起点列 | 过程区列（≥60 列 = col6） | 与组头同列（同一过程区前缀） |
| 行首竖线 | 仅展开体续行 | 每行都有（块内从属） |
| Verb | bold + `text.primary` | 无 bold + `text.primary` |
| 状态符号 | 状态色（`status.running/success/error`） | running/success → `text.dim`；error → `status.error` |
| 错误词 ` — Failed` | 有 | 有（P3） |
| 摘要 | `text.muted` | `text.muted`（一致） |
| duration 右对齐 | `term_width - 1` | `term_width - 1`（一致） |
| 可独立折叠 | 是（Collapsed↔Expanded） | 否（随 subagent 块整体折叠） |
| 点击命中 | 是（切换折叠） | 否（命中归 subagent 顶层行，Enter 打开详情面板） |
| 语义复制 | `{Verb} {summary}{suffix}` | `{Verb} {summary}`（无 suffix、无符号、无缩进） |

差异化由「结构（缩进+竖线）+ 权重（无 bold）+ 音调（dim 符号）」三维叠加构成，
任一维度在 NO_COLOR / 窄屏下丢失时，其余维度仍保持可辨（状态不能只依赖颜色）。

## 5. 状态矩阵

| 状态 | 符号 | 符号色 | 其他信号 |
| --- | --- | --- | --- |
| running | braille 帧（100ms 推进） | `text.dim` | duration 秒数 `4s`（右对齐） |
| completed | `✓` | `text.dim` | duration 冻结值（一位小数秒，`0.4s`；< 50ms 不显示） |
| error | `×` | `status.error` | ` — Failed` 错误词 + 原因行（muted 正文） |

- 无工具时（subagent 刚启动、子工具尚未路由）回退单行 activity 摘要
  （`render_subagent_group_lines` 的 running 分支）。
- 失败原因行 `subagent_error_reason_lines` 与工具行同列（过程区续行前缀），
  与工具行同属一层级；正文保持 muted 不整块染红。

## 6. 断点降级

| 断点 | 缩进 | 符号 | duration | 错误词 |
| --- | --- | --- | --- | --- |
| Wide ≥ 100 | 2 格 | 完整（braille/✓/×） | 右对齐 | 有 |
| Standard 60–99 | 2 格 | 完整 | 右对齐 | 有 |
| Compact 40–59 | 0 格（过程区前缀回退基准形态） | 完整 | 隐藏（非关键 duration） | 有 |
| Narrow < 40 | 0 格（同上） | 退化：符号位省略（`│  Read  src…`） | 无 | 有 |

Narrow 下符号位省略与主时间线「accent 退化为 bullet」是同一降级哲学——
极端窄屏接受状态字符丢失，错误信号由错误词与原因行兜底。

## 7. 动画规则

- running 符号 = `braille_frame(anim_tick())`，10 帧/100ms，与主时间线 running 工具同一
  帧序列、同一 `anim_frame` 缓存重建机制（`VmCacheSlot.anim_frame`，10Hz 驱动）。
- 显著度分层：主时间线 running = `status.running` 色；subagent 工具行
  running = `text.dim` 色。两者都动，但「高显著 spinner」只可能是顶层工具。
- ASCII 终端：降级为 `*`，不再区分显著度。

## 8. 语义复制

- 选中 subagent 工具行复制 → `{Verb} {summary}`（与主时间线 tool header 语义同口径，
  无符号、无 duration、无竖线/缩进）。
- 实现要求：`semantic_line_text` / `strip_visual_prefix` 需识别新前缀结构
  （`[outer 空][indent 留空][标记][gap]` + Verb），对 `TuiSubAgentGroup` 变体剥除缩进与符号列。
- 原因行复制 → 纯错误正文（前缀已剥离）。

## 9. 交互契约（不新增）

- 折叠：subagent 工具行无独立折叠键（`fold_key_of` 对 SubAgent 只认顶层）；
  Enter 在顶层行 → 打开 subagent 详情面板（`SubAgentDetail`），工具行不响应。
- 点击：命中区域 = entry 逻辑首行，即 subagent 顶层行；工具行让位给文本选区。
- 详情面板：面板内是完整嵌套 transcript，工具**按主时间线完整口径渲染**——面板是该
  agent 的「主时间线」，压缩形态的弱化不适用于展开形态。两者自然分工：
  **压缩 = 弱化嵌套行；展开 = 完整口径**。

## 10. 实现位置

- `peri-tui/src/kit/message_area/render/group.rs`：`render_subagent_group_lines`（组装配）、
  `subagent_tool_line`（前缀、无 bold label、dim 符号、错误词、duration 预算）、
  `subagent_error_reason_lines`（原因行同列）。
- `peri-tui/src/kit/message_area/render/helpers.rs`：`process_cont_prefix` 承担缩进与竖线；
  `place_meta` 负责右对齐 duration。
- `peri-tui/src/kit/message_area/render/semantic.rs`：`TuiSubAgentGroup` 变体的
  `semantic_line_text` 剥除竖线、缩进与符号列。

不变项：`render_subagent_group_lines` 顶层行形态、`SUBAGENT_TOOL_LINES`、
braille 动画机制、`place_meta` 右对齐、`fit_summary_to_content`。

## 11. 验证

行为与语义复制回归位于 `peri-tui/src/kit/message_area/render_subagent_groups_test.rs`
（`test_subagent_running_shows_recent_tool_lines`、
`test_subagent_tool_line_narrow_omits_symbol`、
`test_subagent_running_with_failed_tool_shows_reason`、
`test_subagent_failed_reason_line_and_completed`、
`test_subagent_without_tools_keeps_detail_entry_across_statuses`）与
`render_semantic_copy_test.rs`（`test_semantic_subagent_tool_line`、
`test_semantic_subagent_without_tools_copies_entry_and_error_reason`）。

```bash
cargo test -p peri-tui --lib -- subagent
```
