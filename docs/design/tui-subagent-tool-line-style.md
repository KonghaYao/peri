# TUI Subagent 工具行展示设计 —— 与主时间线工具的差异化

状态：proposal（待评审）
关联规范：`tui-chat-message-flow-style.md`（§3.1 网格 / §4.1 符号层级 / §6.4 主时间线工具 / §6.7 Subagent / §8.2 动画 / §9 复制 / §11 断点 / §12 无障碍）
实现现状：`peri-tui/src/kit/message_area/render.rs`（`subagent_tool_line`、`render_subagent_group_lines`、`subagent_error_reason_line`）

## 1. 背景与目标

当前 subagent 运行中展示的「最近工具调用行」与主时间线工具卡片是**同一视觉口径**
（`render.rs` `subagent_tool_line` 注释原文：「与主时间轴 tool activity row 同一视觉口径——
label bold primary，summary muted 暗色，duration 右对齐」）。

```text
// 现状：两处视觉无法区分
   ✓ Read  src/main.rs                              37ms    ← 主时间线工具
   ✓ Read  src/main.rs                              37ms    ← subagent 工具行（无差异）
```

问题：subagent 工具调用是**嵌套过程细节**，不是 transcript 顶层的独立事件。两者同口径导致：

1. **层级不可辨**——无法一眼看出某工具调用发生在哪个 subagent 之内；
2. **权重错位**——子任务的每一行都与主任务工具同等醒目，抢走顶层 entry 的视觉焦点；
3. **噪声放大**——并行 subagent（§6.7 允许多个各占一行）时，满屏同样的工具行更难扫读。

目标：subagent 工具行获得**明确区别于主时间线工具**的展示形态，体现「从属、弱化、可辨状态」，
且不改变交互契约（折叠/点击/复制）、不破坏网格与符号体系、通过 NO_COLOR 与窄屏降级。

## 2. 设计原则

| # | 原则 | 含义 |
| --- | --- | --- |
| P1 | 从属可视 | 工具行结构上必带「续行竖线 + 缩进」，与 subagent 块同属一列层级；正文起点与顶层 entry 不同 |
| P2 | 权重弱化 | label 去 bold；状态符号 dim 化——「bold + 状态色」是主时间线工具的专属锚点 |
| P3 | 错误不弱化 | error 是例外：符号与错误词升级到与主时间线同等显著（错误信息绝不因嵌套而看不清） |
| P4 | 动态一致 | running 符号仍用 braille 动画帧（§8.2 用户已确认「全部 loading 动态」）；低显著通过**颜色**而非动画有无表达 |
| P5 | 交互不变 | 工具行不可独立折叠、点击命中仍为 subagent 顶层行、语义复制仍输出 `{Verb} {summary}` |
| P6 | 锚点守恒 | 每屏每行最多一个 bold 主锚点：subagent 名（顶层行）与主时间线工具 label 是 bold；嵌套工具行不是 |

P2 与 P4 共同落实 §8.2「同屏最多一个高显著 spinner」：高显著 = `status.running` 色动画帧（主时间线），
低显著 = `text.dim` 色动画帧（subagent 工具行）——两者都动，但显著度分层。

## 3. 形态规格（Wide / Standard）

```text
   ◐ Agent explorer  Inspecting message flow          6 tools   ← subagent 顶层行（现状不变）
   │   ⠋ Read   src/main.rs                             4s      ← 嵌套工具行（新形态）
   │   ✓ Bash   cargo test -p peri-tui                 37ms
   │   × Edit   render.rs — Failed
   │   - Error: File not found at src/render.rs                ← 失败原因行（缩进对齐工具行）
```

逐列结构（Span 序列）：

| 段 | 内容 | 样式 |
| --- | --- | --- |
| ① 续行前缀 | `[outer 空][dim 竖线][gap]`（`cont_prefix`，与 subagent 块续行同列） | `sem.text.dim` 竖线 |
| ② 缩进 | `  ` 固定 2 格（常量 `SUBAGENT_TOOL_INDENT = 2`） | 无样式 |
| ③ 状态符号 | running=braille 帧 / success=`✓` / error=`×` | running/success → `sem.text.dim`；error → `sem.status.error` |
| ④ Verb | `format_tool_name` 本地化动词（Read/Bash/Shell/Edit…） | `sem.text.primary`，**无 bold** |
| ⑤ 摘要 | `input_summary` 截断（预算减去缩进与 duration 列） | `sem.text.muted` |
| ⑥ 错误词 | error 时 ` — Failed`（i18n `msg-status-failed`，同 §6.4） | `sem.status.error` + bold |
| ⑦ duration | running 秒数 / completed 冻结值；右对齐 `term_width - 1` | `sem.text.dim` |

要点：

- **缩进固定 2 格而非对齐 subagent 摘要**——流式下最近 3 行持续轮换，固定缩进保证前缀列稳定
  （与 `SUBAGENT_NAME_WIDTH = 16` 定宽同理，§6.7「running 摘要更新不改前缀列」）。
- **不用 `├─`/`└─` 树状末梢字符**——最近 3 行窗口滚动时末梢字符会跳动闪烁，且与现有
  「竖线续行」体系冲突；统一竖线 + 缩进即可表达层级，且实现与 wrap_map/语义复制零摩擦。
- ②③④⑤⑥⑦ 共用 `first_prefix` 之外的构造路径：全部行（含首行）走 `cont_prefix` 风格，
  使 subagent 工具行**永远不是独立 entry 首行**——结构上即从属。

## 4. 与主时间线工具对照

| 维度 | 主时间线工具（§6.4） | subagent 工具行（本设计） |
| --- | --- | --- |
| 正文起点列 | content 列（块首行前缀后） | content + 2（缩进） |
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
任一维度在 NO_COLOR / 窄屏下丢失时，其余维度仍保持可辨（§12：状态不能只依赖颜色）。

## 5. 状态矩阵

| 状态 | 符号 | 符号色 | 其他信号 |
| --- | --- | --- | --- |
| running | braille 帧（100ms 推进） | `text.dim` | duration 秒数 `4s`（右对齐） |
| completed | `✓` | `text.dim` | duration 冻结值 `37ms` |
| error | `×` | `status.error` | ` — Failed` 错误词 + 原因行（muted 正文） |

- 无工具时（subagent 刚启动、子工具尚未路由）回退单行 activity 摘要——现状行为不变
  （`render_subagent_group_lines` 的 running 分支已处理）。
- 失败原因行 `subagent_error_reason_line` 的缩进与工具行对齐（content + 2），
  与工具行同属一层级；正文保持 muted 不整块染红（§6.7 现状口径）。

## 6. 断点降级（§11）

| 断点 | 缩进 | 符号 | duration | 错误词 |
| --- | --- | --- | --- | --- |
| Wide ≥ 100 | 2 格 | 完整（braille/✓/×） | 右对齐 | 有 |
| Standard 60–99 | 2 格 | 完整 | 右对齐 | 有 |
| Compact 40–59 | 2 格 | 完整 | 隐藏（§11 非关键 duration） | 有 |
| Narrow < 40 | 2 格 | 退化：符号位省略（`│  Read  src…`） | 无 | 有 |

Narrow 下符号位省略与主时间线「accent 退化为 bullet」是同一降级哲学（§11）——
极端窄屏接受状态字符丢失，错误信号由错误词与原因行兜底。

## 7. 动画规则

- running 符号 = `braille_frame(anim_tick())`，10 帧/100ms，与主时间线 running 工具同一
  帧序列、同一 `anim_frame` 缓存重建机制（`VmCacheSlot.anim_frame`，10Hz 驱动）。
- 显著度分层（§8.2 落实）：主时间线 running = `status.running` 色；subagent 工具行
  running = `text.dim` 色。两者都动，但「高显著 spinner」只可能是顶层工具。
- ASCII 终端：降级为 `*`（§4.1 降级表），不再区分显著度。

## 8. 语义复制（§9）

- 选中 subagent 工具行复制 → `{Verb} {summary}`（与主时间线 tool header 语义同口径，
  无符号、无 duration、无竖线/缩进）。
- 实现要求：`semantic_line_text` / `strip_visual_prefix` 需识别新前缀结构
  （`cont_prefix + 2 缩进 + 符号 + Verb`），对 `TuiSubAgentGroup` 变体剥除缩进与符号列。
- 原因行复制 → 纯错误正文（现状已剥离前缀，缩进后同样剥离）。

## 9. 交互契约（延续现状，不新增）

- 折叠：subagent 工具行无独立折叠键（`fold_key_of` 对 SubAgent 只认顶层）；
  Enter 在顶层行 → 打开 subagent 详情面板（`SubAgentDetail`），工具行不响应。
- 点击：命中区域 = entry 逻辑首行（§9.1），即 subagent 顶层行；工具行让位给文本选区。
- 详情面板：面板内是完整嵌套 transcript，工具**按主时间线完整口径渲染**——面板是该
  agent 的「主时间线」，压缩形态的弱化不适用于展开形态。两者自然分工：
  **压缩 = 弱化嵌套行；展开 = 完整口径**。

## 10. 实现落点

文件：`peri-tui/src/kit/message_area/render.rs`

| 位置 | 改动 |
| --- | --- |
| 新增常量 | `SUBAGENT_TOOL_INDENT: usize = 2`（紧邻 `SUBAGENT_TOOL_LINES`） |
| `subagent_tool_line` | ① 前缀改为 `cont_prefix` + 2 空格缩进（替代 `first_prefix`）；② label 去 `Modifier::BOLD`；③ 符号色：running/success 用 `sem.text.dim`，error 用 `sem.status.error`（不能直接复用 `status_symbol_and_color` 的色，需参数化或就地处理）；④ error 补 ` — Failed` 错误词（复用 §6.4 的 `msg-status-failed` 逻辑）；⑤ summary 预算与 `place_meta` 的 `used` 计算自然含缩进宽度，budget 需再减 `SUBAGENT_TOOL_INDENT`；⑥ Narrow 断点符号位省略（§6 断点表） |
| `subagent_error_reason_line` | 前缀改为 `cont_prefix` + 2 空格缩进（与工具行同列） |
| `semantic_line_text` | `TuiSubAgentGroup` 变体：子工具行语义 `{Verb} {summary}`，剥竖线+缩进+符号 |
| `render_test.rs` | 更新 subagent 相关断言：前缀宽度、无 bold、符号 dim 色、error 词、缩进列 |

不变项：`render_subagent_group_lines` 顶层行形态、`SubAgentSummary` 派生、
`SUBAGENT_TOOL_LINES = 3`、braille 动画机制、`place_meta` 右对齐、`fit_summary_to_content`。

## 11. 测试要点

1. **层级结构**：subagent 工具行首列 = `[outer 空][│][gap][2 空格]`，正文起点 = `content + 2`；
2. **bold 缺失**：工具行 Verb span 无 `Modifier::BOLD`（对照主时间线工具行有）；
3. **符号色**：running/success 符号 `fg == text.dim`；error 符号 `fg == status.error`；
4. **错误词**：error 工具行含 ` — Failed`（bold + error 色），原因行缩进与工具行一致；
5. **断点矩阵**：Compact 无 duration；Narrow 无符号位且不 panic（宽度 1 回归）；
6. **语义复制**：选中工具行 → `{Verb} {summary}`，无 `✓`/`│`/前导空格；
7. **NO_COLOR**：剥离颜色后仍可辨（缩进 + 竖线 + 符号字符 + bold 有无）。

## 12. 验收标准

- [ ] subagent 运行中展示最近 ≤3 工具行，形态与主时间线工具卡片肉眼可辨（缩进/无 bold/dim 符号）；
- [ ] running 工具行动画推进（braille 帧），色彩 dim 但仍动态；
- [ ] error 工具行错误词 + 原因行不弱化；subagent 顶层行形态零改动；
- [ ] 120/80/48 列与 NO_COLOR 下 golden 场景通过；语义复制输出无 chrome；
- [ ] Enter 打开详情面板、点击命中、折叠行为与现状完全一致；
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 与 peri-tui 全量测试通过。
