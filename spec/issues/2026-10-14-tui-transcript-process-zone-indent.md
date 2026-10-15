# TUI 消息区：过程区缩进与转场台阶

- 状态：已实施（2026-10-14）；本文件是本轮改动的唯一事实源（几何、决策、验收）。
- 日期：2026-10-14。
- 范围：`peri-tui` 消息区主时间轴渲染（`src/kit/message_area/`、`src/kit/entry_render_cache.rs`、`src/kit/panels/subagent_detail.rs` 的网格构建）。
- 性质：需求澄清与两轮形态修订后的实施规格。几何与决策均经用户确认，实施时不再自行改口径。
- 形态定案与修订史：`spec/issues/2026-10-14-tui-transcript-shape-review.md`（转角字形定圆角、连接定 1 行台阶）。
- 权威依据：`peri-tui/CLAUDE.md`、`docs/design/tui-chat-workbench.md`、`docs/standards/tui.md`、`docs/standards/architecture-contracts.md`（`ARC-BOUNDARY-001`、`ARC-EVENT-001`）。

## 目标

消息区时间轴分层：用户/AI 正文留在基准列；所有非文本 entry（下文称**过程区**）右移 2 格；文本区与过程区的转场由**转场后首个 entry 的台阶**承担——进入 ` ╰─╮`（col1 `╰` / col2 `─` / col3 `╮`）、退出 ` ╭─╯`（镜像），各占 1 行、宽 4，接点落在 `│` 竖线的中心线上。

```text
 │  我先确认目标形态再动手。
 ╰─╮
   ✓  Read peri-tui/…/helpers.rs — 113 lines                     0.2s
   │  use crate::kit::message_area::grid::{…};
   │  Thought for 501s · 38 lines
   │  Micro compaction completed (…)
 ╭─╯
 │  结论如下……
```

## 布局规格（列坐标口径）

1. **断点矩阵**：`Wide/Standard`（≥60 列）`indent = 2`；`Compact`(40–59) 与 `Narrow`(<40) `indent = 0`——保持现状（无缩进、无台阶）。
2. **文本行**：`[outer 1][accent 1][gap 2]` → 内容 col4（现状不变）。
3. **过程区首行**：`[outer 1][indent 2 空格][标记 1 格][gap 2]` → 正文 col6（与 v1 同列，正文列不因形态修订移动）。标记 = 状态符号（工具行 icon）或 `│`（reasoning / 注记 / 续行），落在 `process_marker_col() = outer + indent`（col3）。
4. **过程区续行**：`[outer 1][indent 2 空格][│][gap 2]` → 内容 col6，与首行标签同列。
5. **首行与续行前缀同宽**（`process_prefix_width() = process_marker_col + accent + gap`，indent=2 时 = 6）⇒ wrap map、可视高度、列映射不因行种类变化；**台阶行宽 4、不含内容**，是否出现不影响 wrap map 与高度（台阶是独立行）。
6. **转场台阶**（`ZoneTransition`）：进入 = 文本区 → 过程区，退出 = 过程区 → 文本区；由**转场后的首个 entry** 在行首插入（后向依赖：只看前一个「产出行 entry」的终点 zone，追加 entry 不回溯翻转已渲染 slot）。首个 entry、同区接续、转场后 entry 渲染 0 行 → 无台阶。`AssistantBubble` 内部 reasoning → 正文的退出台阶由该 entry 渲染时自画（entry 内部事务，无跨 slot 依赖）。
7. **entry 起止 zone**：
   - `TuiUserBubble` = 文本/文本（user 永不缩进，不参与短回复判定）；
   - `TuiAssistantBubble` = 起点（有 reasoning 或**短回复** → 过程，否则文本），终点（正文非空且**非短回复** → 文本，否则等于起点）；
   - `TuiDivider` = 文本/文本——turn 边界装饰属文本区；后续 user prompt 前固定画 `├──…` 横线与一行空白，不参与过程区缩进与台阶；
   - 渲染 0 行的 entry 不参与相邻判定；
   - 其余 entry（ToolCard / SubAgentGroup / CollapsedGroup / SystemNote / SystemReminder / AskUserBlock / TodoSummary）= 过程/过程。
8. **右缘不动**：`place_meta` 仍右对齐到 `line_width()`（带右缘，跳过滚动条列）；过程区正文可用宽度 = `content - indent`；`band_width` 与整带居中不变。
9. **`indent == 0` 回退**：过程区前缀回退为基准形态（符号在 accent 列、gap 1），不画台阶——Compact/Narrow 必须与改动前逐列一致。
10. **字形口径**：台阶 `╰`(U+2570) `╮`(U+256E) `╭`(U+256D) `╯`(U+256F) 与连接线 `─`(U+2500)、标记 `│`(U+2502) 同为硬编码结构字形，不新增 `SymbolSet` 字段；台阶字形集中在 `render/helpers.rs` 的 `STEP_ENTER` / `STEP_EXIT` 常量。
11. **索引顺延**：入口台阶由 `vm_to_lines_cached_with_layout` 在 slot 行首插入，使该 slot 内 copy button / interaction 选项行 / image 行的 `logical_idx` 与 markdown stable overlay 起点同步 +1；animation 原位刷新按 `running_header_index`（台阶字形判定）定位 running header。
12. **语义复制**：台阶行是纯结构 chrome——提取层（`selection.rs`）整行跳过，`semantic_line_text` 对该行返回空串；`local_idx` 口径为「VM 自身行序」（不含台阶顺延），提取层按台阶占位校正后传入。
13. **短回复并入过程带**（`TuiAssistantBubble::is_brief`：`text.chars().count() < BRIEF_REPLY_MAX_CHARS`
    = 120，用户拍板）：正文行前缀走过程区形态（`process_prefixed_cont_line`）、markdown / 表格
    wrap 宽度取 `process_content_width()`——**AI 短回复缩进，human 永远不缩进**。zone 判定
    （`transcript::{start_zone,end_zone}`）与渲染前缀共用 `is_brief`（单一权威）→ 与相邻过程
    entry 同区接续（进出都不画台阶），气泡内 reasoning → 短回复正文也不画内部台阶。
    `indent == 0`（Compact/Narrow）经 `process_*` 回退为基准形态，与长回复逐列一致。
    空正文（纯 reasoning / 工具轮）恒为短回复（不改变既有形态——本就走过程区）。
    短回复不画台阶，正文上下各保留 1 行过程带前缀空行作视觉分隔（与 Compact/Narrow
    的节拍空行同形，见决策 11）。
    阈值依据：43.17 万条 assistant 消息正文长度中位 68 字、P75 = 157 字，分布无天然双峰谷底
    （P25 = 36 / P90 = 649），120 是产品取舍而非数据断点。

## 决策清单（用户确认，不再复议）

1. 文本不动；范围为除 `TuiUserBubble` 与 `TuiAssistantBubble` 正文外全部 entry（含气泡内 reasoning、系统提醒/注记、提问块、Todo 摘要、折叠组）；`TuiDivider` 属文本区。
2. 缩进对象是**工具块整体**：状态符号不再占 accent 列，改占过程区左缘标记列（col3）；col1 留空作为台阶接点。
3. 转场形态：进入与退出各画 1 行台阶（圆角字形），不在竖线上分叉、不在 entry 之间画斜线。
4. 缩进量：统一 +2；<60 列 +0。
5. 子 Agent 组内工具行**合并为总 +2**：删除了 `SUBAGENT_TOOL_INDENT`，组内行与组头同列。
6. 首行符号在 indent 之后（col3），标签与续行内容同列（col6）。
7. 范围仅消息区主时间轴；子 Agent 详情 / shell 详情面板保持现状（`with_content` 置 `indent = 0`）。
8. 不加开关、配置项或环境变量。
9. 注记（Info）不画 `── label ──` 横线——走普通过程行：左缘标记列 `│` + 正文（与 reasoning 摘要同形）。
10. 台阶行不进入复制文本、不保留空行。
11. Wide/Standard 的文本块首尾不再保留节拍空行（§3.2）——块分隔由转场台阶承担；
    Compact/Narrow 无台阶，保留正文块上下各 1 空行（逐列与改动前一致）；短回复（决策 13）
    无台阶可用，正文上下同样各保留 1 空行（过程带前缀，与 Compact/Narrow 同形）。
12. 台阶线色随所接文本的左侧线条——进入台阶接上方文本 entry、退出台阶接下方文本 entry
    （user → `text.secondary`、assistant 正文 → `accent`、Divider → `text.dim`，与
    `── label ──` 同色）；entry 内部（reasoning → 正文）台阶用正文色。转场处颜色不断链。
13. 短回复阈值：< 120 字（`chars().count()` 口径，用户拍板；依据见布局规格 §13）——AI
    回复少于该字数时正文并入过程带（缩进），human 永远不缩进。不改开关/配置项（决策 8）。
14. 转场台阶与过程带形态的门控是**阈值 + 缩进**两条正交条件：短回复只在 `indent > 0`
    （Wide/Standard）改变列位；`indent == 0` 时判定仍在（zone 一致），渲染逐列与改动前一致。

## 实现落点

- `message_area/grid.rs`：`indent` 字段与断点赋值；`process_marker_col()`、`process_prefix_width()`（`indent == 0` 取 `first_prefix_width()`）、`process_content_width()`；`TextLineSource`（台阶线色来源）；`ZoneTransition { None, EnterProcess(TextLineSource), ExitProcess(TextLineSource) }`。
- `message_area/render/helpers.rs`：`process_first_prefix`（`indent == 0` 回退 `first_prefix`）、`process_cont_prefix`；`STEP_ENTER` / `STEP_EXIT` / `step_line(text, color)` / `text_line_color(source)` / `is_step_line`。
- `message_area/render.rs`：`vm_to_lines_cached_with_layout` 收 `transition` 入参——非 None 且在 slot 行首插入台阶并顺延各索引；`TuiAssistantBubble` 分支自画内部退出台阶；`running_header_index` / `is_step_row`。
- `message_area/transcript.rs` / `transcript_index.rs`：`start_zone` / `end_zone` / `produces_lines` / `zone_transition`；按需物化与常驻渲染同用此判定。
- `entry_render_cache.rs`：`transition` 纳入缓存失效——转场形态翻转触发该 slot 的 Layout 级重渲染，不影响其他 slot。
- `message_area/render/{tool_card,group,reasoning,system,interaction,error,user,semantic}.rs`：前缀切换到 `process_*`、宽度预算切 `process_content_width()`；语义剥离按 span 结构判定并识别台阶行。
- `message_area/selection.rs`：台阶行整行跳过 + `local_idx` 台阶顺延。
- `message_area/render/user.rs` / `render.rs`：文本块节拍空行仅在 `indent == 0`（Compact/Narrow）保留；Wide/Standard 由台阶承担块分隔；短回复（`brief`）无台阶，正文上下各 1 空行经 `body_line` 走过程带前缀（条件 `indent == 0 || brief`）。
- `kit/tui_render_unit/bubble.rs`：`BRIEF_REPLY_MAX_CHARS`（= 120）与 `TuiAssistantBubble::is_brief()`——zone 判定与渲染前缀共用的单一权威。
- `message_area/render.rs`（短回复路径）：`body_width`（短回复取 `process_content_width()`，否则 `content_width()`）与 `body_line`（短回复走 `process_prefixed_cont_line`）统一供 markdown / 表格 / 图片 / 段间空行使用；内部台阶条件加 `!brief`。
- `message_area/render/helpers.rs`：`process_prefixed_cont_line`（`indent == 0` 回退 `prefixed_cont_line`）。
- `message_area/transcript.rs`：`start_zone` / `end_zone` 按 `is_brief` 判过程区。

## 验收

- [x] ≥60 列：过程区标记落 col3、正文 col6；工具行 icon 与 reasoning / 注记 / 续行 `│` 同列。
- [x] 台阶仅出现在文本区 ↔ 过程区边界；同区接续、时间轴首个 entry、0 行 entry 前后均无台阶。
- [x] 气泡内 reasoning → 正文的退出台阶由气泡自画；纯 reasoning 块（无正文）不画退出台阶。
- [x] <60 列：与改动前逐列一致（前缀回退基准形态、无台阶；回退路径由测试逐列断言）。
- [x] 语义复制剥净前缀与台阶；面板（子 Agent 详情）渲染逐列不变。
- [x] 台阶线色 = 所接文本左侧线条色（进入台阶接上方文本、退出台阶接下方文本；user → `text.secondary`、assistant → `accent`、divider → `text.dim`；entry 内部台阶用正文色）——`test_step_line_color_follows_text_line`。
- [x] 文本块内所有行保持竖线前缀（含 md 复制按钮行，右对齐与点击区域不变），左缘时间线不断链——`test_assistant_bubble_renders_copy_button`。
- [x] Divider 保持文本区形态；现为后续 prompt 前 `├──…` 横线与一行空白；Info 注记走过程行 `│` + label。
- [x] Wide/Standard 正文块（user 气泡 / assistant 正文）首尾无节拍空行；Compact/Narrow 保留上下各 1 空行（回退路径由测试逐列断言）；短回复（无台阶）上下各 1 行过程带前缀空行——`test_vertical_rhythm_wide_and_compact` / `brief_reply_stays_in_process_zone_without_step`。
- [x] 本轮重验：`test --locked -p peri-tui --lib` 1849 passed / 0 failed / 6 ignored（与基线一致）；layer-imports 22 条规则 0 违规；`git diff --check` 干净；check --all-targets / clippy / fmt / doc 见下方验证命令。
- [x] 按 `DOC-UPDATE-001` 核对受影响路由与事实源；不新增平行文档（`docs/design/tui-chat-workbench.md`、`docs/design/tui-subagent-activity.md`、`docs/design/tui-streaming-markdown-performance.md`、`docs/code-index/peri-tui.md` 已同步）。
- [x] 短回复（<120 字）正文并入过程带（前缀 col6、wrap 宽度 `process_content_width`）；与相邻过程 entry 同区接续（进出无台阶），气泡内 reasoning → 短回复正文无内部台阶——`brief_reply_stays_in_process_zone_without_step` / `test_step_line_color_follows_text_line`。
- [x] user 气泡不参与短回复判定（永远留在文本区）——`test_message_line_colors_follow_role_tokens` / `test_text_zone_baseline_and_process_zone_indent_columns`。
- [x] <60 列短回复与长回复逐列一致（无缩进、基准前缀、无台阶）——`test_compact_and_narrow_process_prefix_falls_back_to_baseline`。
- [x] 语义复制在短回复 / 长回复两种形态下剥前缀结果一致——`test_semantic_plain_lines_strip_prefix` / `test_extract_semantic_plain_middle_line`。
- [x] 短回复轮重验：`test --locked -p peri-tui --lib` 1851 passed / 0 failed / 6 ignored；check --all-targets / clippy / fmt / `git diff --check` 见下方验证命令。

## 与 v1 形态的差异（本轮修订，v1 已提交于 `a3f8f156b`）

1. 进入连接：`├──`（分叉符 + 填充）→ 1 行台阶 ` ╰─╮`；新增退出台阶 ` ╭─╯`（v1 退出不画符）。
2. 过程区左缘：`[outer][│ 或 ├][fill]` → `[outer][indent 留空][标记]`——col1 竖线让给台阶，标记移到 col3；正文列不变。
3. `ProcessLead { Branch, Inline }` → `ZoneTransition { None, EnterProcess, ExitProcess }`。
4. `TuiDivider` 由过程区改归文本区；Info 注记由 `── label ──` 改过程行 `│` + label。
5. 台阶行引入的索引顺延与语义复制跳过（上文 §11/§12）。
6. Wide/Standard 文本块首尾节拍空行删除（v1 保留上下各 1 空行）；Compact/Narrow 保留；短回复（无台阶）上下各补 1 行过程带前缀空行。

## 验证命令

```bash
./scripts/cargo-rmcp-patched.sh check --locked -p peri-tui --all-targets
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
./scripts/cargo-rmcp-patched.sh clippy --locked -p peri-tui --all-targets
./scripts/cargo-rmcp-patched.sh fmt --all --check
git diff --check
```

## 风险

1. 缓存失效：转场形态翻转必须重渲染对应 slot（相邻 entry 由文本变过程或反之），但不整表重建。
2. `transcript.rs::produces_lines` 与 render 层 0 行早返回必须同源——两处不同步会让台阶出现在 0 行 entry 之后。
3. 语义剥离按 span 结构判定：过程区前缀（标记前是 indent 空格段）与文本区前缀（accent 1 格）不互认，新增行种须同时更新判定。
4. 短回复阈值是 zone 判定与渲染前缀的共享输入：只经 `TuiAssistantBubble::is_brief`（`BRIEF_REPLY_MAX_CHARS`）单点维护——两处各写一份长度比较会出现「台阶与列位不一致」的形态漂移。
