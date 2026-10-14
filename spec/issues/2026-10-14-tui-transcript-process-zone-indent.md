# TUI 消息区：过程区缩进与进入分叉

- 状态：已实施（2026-10-14，S1–S5 与随后修订已落地）；本文件是本轮改动的唯一事实源（几何、决策、验收）。
- 日期：2026-10-14。
- 范围：`peri-tui` 消息区主时间轴渲染（`src/kit/message_area/`、`src/kit/entry_render_cache.rs`、`src/kit/panels/subagent_detail.rs` 的网格构建）。
- 性质：5 轮需求澄清后的实施规格。几何与决策均经用户确认，实施时不再自行改口径。
- 权威依据：`peri-tui/CLAUDE.md`、`docs/design/tui-chat-workbench.md`、`docs/standards/tui.md`、`docs/standards/architecture-contracts.md`（`ARC-BOUNDARY-001`、`ARC-EVENT-001`）。

## 目标

消息区时间轴分层：用户/AI 正文留在基准列；所有非文本 entry（下文称**过程区**）整体右移 2 格；进入过程区的首行画分叉 `├──`，退出不画；主竖线贯穿不换列。

```text
 │ 文本行（基准列 col4）
 ├──✓ Read(src/main.rs)      ← 进入行：col1 ├，col2–3 ──，col4 符号，col5 空格，col6 标签
 │    fn main() {            ← 过程区续行：col1 │，col2–5 空格，col6 内容
 │ 文本继续（col4，无退出符）
```

## 布局规格（列坐标口径）

1. **断点矩阵**：`Wide/Standard`（≥60 列）`indent = 2`；`Compact`(40–59) 与 `Narrow`(<40) `indent = 0`——保持现状（无缩进、无分叉）。
2. **文本行**：`[outer 1][accent 1][gap 2]` → 内容 col4（现状不变）。
3. **过程区首行**：`[outer 1][│ 或 ├][fill = indent 格][符号 1 格][空格 1 格]` → 标签 col6。分叉行 fill 用 `─`，区内行 fill 用空格。
4. **过程区续行**：`[outer 1][│][indent + 2 空格]` → 内容 col6，与首行标签同列。
5. **首行与续行前缀同宽**（`indent + 4`）：分叉行与区内首行等宽 ⇒ wrap map、可视高度、列映射不因分叉变化。
6. **分叉条件**（`ProcessLead::Branch`）：entry 起点为过程区，且前一个「产出行的 entry」终点为文本区；否则 `Inline`。首个 entry 恒 `Inline`（避免悬空分叉）。
7. **entry 起止 zone**：
   - `TuiUserBubble` = 文本/文本；
   - `TuiAssistantBubble` = 起点（有 reasoning → 过程，否则文本），终点（正文非空 → 文本，否则等于起点）；
   - 渲染 0 行的 entry 不参与相邻判定；
   - 其余 entry（ToolCard / SubAgentGroup / CollapsedGroup / SystemNote / SystemReminder / Divider / AskUserBlock / TodoSummary）= 过程/过程。
8. **右缘不动**：`place_meta` 仍右对齐到 `line_width()`（带右缘，跳过滚动条列）；过程区正文可用宽度 = `content - indent`；`band_width` 与整带居中不变。
9. **`indent == 0` 回退**：过程区前缀回退为现状形态（符号在 accent 列、gap 2），不画分叉——Compact/Narrow 必须与改动前逐列一致。
10. **字形口径**：分叉 `├`(U+251C) 与填充 `─`(U+2500) 沿用与竖线 `│` 相同的硬编码结构字形口径，不新增 `SymbolSet` 字段（现状 `│`/`─`/divider 亦为硬编码）。

## 决策清单（用户确认，不再复议）

1. 文本不动；范围为除 `TuiUserBubble` 与 `TuiAssistantBubble` 正文外全部 entry（含气泡内 reasoning、分隔线、系统提醒/注记、提问块、Todo 摘要、折叠组）。
2. 缩进对象是**工具块整体**：状态符号不再占 accent 列（accent 列退化为贯通的 `│`/`├`）。
3. 转折形态：仅进入分叉（`├──`），退出不画；主竖线贯穿。
4. 缩进量：统一 +2；<60 列 +0。
5. 子 Agent 组内工具行**合并为总 +2**：删除 `SUBAGENT_TOOL_INDENT`，组内行与组头同列。
6. 首行符号在 fill 之后（col4），标签与续行内容同列（col6）。
7. 范围仅消息区主时间轴；子 Agent 详情 / shell 详情面板保持现状。
8. 不加开关、配置项或环境变量。

## 实现步骤

### S1 骨架（grid / helpers / lead 传递）

- `src/kit/message_area/grid.rs`：`GridSpec` 加 `indent: u16`；`grid_for` 按断点赋值（≥60 → 2，否则 0）；`with_content` 置 `indent = 0`；新增 `process_content_width()`（`content - indent`，下限 1）、`process_prefix_width()`（`indent + 4`）；导出 `ProcessLead { Branch, Inline }`；更新模块文档中「所有 entry 共享左起点」的表述为「文本共享基准列，过程区右移 `indent`」。
- `src/kit/panels/subagent_detail.rs`：构建 grid 后置 `indent = 0`（注释：嵌套面板沿用基准列，本 issue 不改变面板形态）。
- `src/kit/message_area/render/helpers.rs`：新增 `process_first_prefix(grid, symbol, style, lead)` 与 `process_cont_prefix(grid, color)`；`indent == 0` 时回退到 `first_prefix` / `cont_prefix` 现状形态。
- `src/kit/message_area/render.rs`：`vm_to_lines_cached_with_layout` 增加 `lead: ProcessLead` 入参并分派；`TuiAssistantBubble` 的 reasoning 段消费 `lead`、正文段保持基准列（退出不画符）；`render/` 下各渲染函数签名加 `lead` 入参（函数体暂保持旧 helper，S2 切换）。
- `src/kit/message_area/transcript.rs`：逐 slot 按相邻 entry 起止 zone 计算 `lead`（重建区间的首 slot 也要读取 `items[i-1]`）。
- `src/kit/entry_render_cache.rs`：`lead` 纳入缓存失效——lead 翻转必须触发该 slot 的 Layout 级重渲染，且不影响其他 slot。
- 出口条件：`cargo check -p peri-tui` 与 `--all-targets` 可编译（测试断言不改，只修签名调用点）。

### S2 渲染器切换（可与 S3/S4 并行）

- A：`render/tool_card.rs`、`render/group.rs`（含删除 `SUBAGENT_TOOL_INDENT`；组内工具行、失败原因行、折叠组嵌套渲染与组头同列）。
- B：`render/system.rs`（note / divider / todo）、`render/interaction.rs`（含 `InteractionLayout.option_cols` 列区间按过程区前缀宽计算）、`render/error.rs`、`render/reasoning.rs`。
- 统一切换：前缀换 `process_*`；宽度预算 `grid.content_width()` → `grid.process_content_width()`；`place_meta` / 右缘不变。

### S3 下游列映射

- `render/semantic.rs`：复制语义必须剥净外层前缀 + 过程区缩进 + 分叉（`│`/`├`/填充空白/`─`），reasoning 行与 md 行都要正确；优先按前缀 span 结构剥离，禁止只用 `local_idx` 定宽猜测。
- `hits.rs` / `selection.rs` / `text_selection.rs` / `render/interaction.rs`：核对「内容列 = col4」类硬编码假设，随渲染行宽与前缀宽修正。

### S4 文档

- `docs/design/tui-chat-workbench.md`：布局规则补充过程区缩进、分叉、断点降级与右缘不变。
- `docs/code-index/peri-tui.md`：涉及渲染入口描述时同步。

### S5 测试

- 新增：
  1. 分叉只在文本区→过程区的首个 entry 出现；连续过程 entry 无分叉；首个 entry 无分叉。
  2. <60 列（Compact/Narrow）渲染与改动前逐列一致（无缩进、无分叉）。
  3. 气泡内 reasoning 段走过程区（分叉/缩进），正文段回基准列。
  4. 复制语义结果不含 `│`/`├`/`──`/缩进空格。
  5. AskUser 选项列区间（`InteractionLayout`）随过程区前缀宽偏移。
  6. 过程区首行与续行前缀同宽；duration 仍右对齐带右缘。
- 更新：现有断言列/前缀的测试（`render_*_test.rs`、`selection_test.rs`、`transcript_test.rs`、`mod_test.rs`、`render_entry_layout_test.rs` 等）。

## 验收

- [x] `./scripts/cargo-rmcp-patched.sh check -p peri-tui`、`... test --locked -p peri-tui --lib` 通过（1849 passed / 0 failed / 6 ignored）；`git diff --check` 干净。
- [x] ≥60 列：过程区标签与续行内容同列（col6），分叉行 col1 = `├`，accent 列不再出现状态符号。
- [x] <60 列：与改动前逐列一致（前缀回退基准形态；回退路径由测试逐列断言）。
- [x] 分叉仅出现在文本区→过程区边界；子 Agent 组头与组内工具行同列；无 `SUBAGENT_TOOL_INDENT` 残留。
- [x] 语义复制剥净前缀与缩进；面板（子 Agent 详情）渲染逐列不变。
- [x] 按 `DOC-UPDATE-001` 核对受影响路由与事实源；不新增平行文档（`docs/design/tui-chat-workbench.md`、`docs/design/tui-subagent-activity.md`、`docs/design/tui-streaming-markdown-performance.md`、`docs/code-index/peri-tui.md` 已同步）。

## 实施修订（S1–S5 之后，与本文件口径一致）

1. `TuiSystemReminder`（§7 已列为过程区 entry，S2 遗漏）：`render/user.rs::render_system_reminder_lines`
   增加 `lead` 入参并切换到 `process_first_prefix` / `process_cont_prefix` /
   `process_content_width()`；气泡**内嵌**提醒（`render_reminder_condensed`，随 UserBubble
   的文本区 zone）保持基准列不动。
2. `render/helpers.rs::fit_summary_to_content`：上限按区取——过程区 =
   `process_prefix_width() + process_content_width()`（正文含前缀不越右缘预算），
   Compact/Narrow 保持 `content_width()` 旧口径（§9 逐列一致）。
   新增调用顺序约定：**先裁正文、再 `place_meta`**（两处调用点已调整）——否则右对齐的
   duration 会成为「最后一个非空白 span」被截成存根（`0.4s` → `0`）。
3. `grid.rs::process_prefix_width()`：`indent == 0` 返回基准前缀宽（`first_prefix_width()`），
   与回退形态的实际渲染宽度一致（此前恒为 `indent + 4`，调用方需自行分支）。

## 验证命令

```bash
./scripts/cargo-rmcp-patched.sh check -p peri-tui
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib
git diff --check
```

## 风险

1. 缓存失效：`lead` 翻转必须重渲染对应 slot（相邻 entry 由文本变过程或反之），但不整表重建。
2. 既有大量列断言测试需要同步更新，更新时必须保持断言强度（不得删断言换通过）。
3. 气泡内混排（reasoning + 正文）使「entry 级 lead」不足以描述行级前缀，复制语义与宽度预算需按段判断。
