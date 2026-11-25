# P1：Mermaid 流式预览在消息区回退源码并拼接未确认尾部

**状态**：已修复。
**优先级**：P1。
**类型**：错误（显示链路）。
**创建日期**：2026-11-24。
**来源**：房间维护方案（TiboDots，seq 40128）+ `fix-bug` 指派。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `cca9a2be6`，worktree `fix/mermaid-tui`）。

## 确认证据

- 消息区每帧先渲染、后观察：`MessageArea` render body 先 `Transcript::prepare`
  （`peri-tui/src/kit/message_area/message_area.rs:252`）→ changed slot `ensure_slot`
  （`transcript.rs:300-307`）→ `EntryRenderCache::ensure`
  （`peri-tui/src/kit/entry_render_cache.rs:67`）→ gated 渲染
  （`entry_render_cache.rs:147-156`）→ `display_lines_peek`
  （`peri-tui/src/kit/mermaid/display.rs:151-174`）；之后才 `warm_visible`
  （`message_area.rs:619`）→ `observe_span`
  （`peri-tui/src/kit/mermaid/mod.rs:82-84`）。
- `PreviewEngine::peek`（`peri-tui-components/src/mermaid/preview.rs:623-655`）只在
  `source_seen == source && width == width` 时返回状态视图；否则只有「已闭合且
  `LayoutKind::Final` 几何精确命中」才返回帧，其余一律 `PreviewContent::Source`。
  **没有任何前缀回退**，也不查询 `LayoutKind::Preview` 几何。
- `FenceState::view`（`preview.rs:332-376`）在有可用帧时把
  `source[self.trusted_end..]` 作为 `tail` 返回；`render_content`
  （`display.rs:183-233`）把 `Source` 直接映射为 `segment.lines`（整段围栏源码），
  并在帧后逐行追加 `tail`（`display.rs:216-224`）。
- `observe` 只在 `state.digest()` 变化时 bump（`preview.rs:615-619`），digest
  不含 `source_seen`/尾部内容（`preview.rs:178-191`），纯追加且无新完整语句时
  不会触发下一次重渲染。
- 终稿失败时清 `frame`/`final_frame`（`preview.rs:820-831`），随后 `view` 返回
  `Source + Failed`，消息区展开整段源码。

主 agent 已逐条复核上述行号（`peek`/`view`/`render_content`/`ensure` 分支与
gated—scheduled 分流），与 explorer 报告一致。

## 根因链

1. 触发：流式 chunk 追加到围栏源码（或终端列宽变化）。
2. 机制：该帧 `peek` 的精确相等条件失败 → 返回 `Source`；下一帧 `observe`
   才会更新状态并排队，但 `peek` 仍不认前缀，且 tail/source_seen 变化不进 digest，
   不保证重渲染。
3. 用户可见后果：消息区在流式期间周期性/持续显示整段原始代码；有帧时图下还会
   拼接未确认源码尾；终稿失败时源码全文展开。观察：代码路径确定；未实测真实
   终端下每次 chunk 的视觉频率（推断：取决于 chunk 频率与 80ms 合并窗口）。

## 影响面

- 用户影响：聊天正文出现原始 Mermaid 源码，图闪烁/被源码顶替；终稿失败的图
  以整段源码占据正文。
- 受影响模块/调用点：`peri-tui-components/src/mermaid/preview.rs`
  （`peek`/`view`/`FenceState`）、`peri-tui/src/kit/mermaid/display.rs`
  （`display_lines_peek`/`render_content`）、
  `peri-tui/src/kit/message_area/render/renderer.rs`（surface 参数）、
  两处 locale 与新占位文案。
- 是否可规避：可以。等围栏闭合看终稿，或打开详情面板查看图/源码。
- 不属于 P0：无数据丢失、无崩溃、无安全问题，主链路可用且有临时规避。

## 修复方案

- 范围：
  1. `PreviewEngine::peek` 增加前缀回退：同一围栏状态存在且
     `source.starts_with(state.source_seen)` 且存在可用帧时，返回**不带 tail**
     的帧视图（宽度不同也复用旧帧，交给现有横向视口裁剪）；保留已闭合
     `Final` 几何快速路径；只有无帧或非前缀才返回 `Source`。
  2. 消息区（chat surface）不再内联源码：`Source` 且无 notice → 单行占位
     「图表生成中」；`Source` 且有 notice → 只显示短原因；`Frame` 不追加 tail；
     末尾「查看图」动作行始终保留。完整源码只在详情面板可见。
  3. 显式传递 surface（`MermaidSurface::{Chat, Detail}`）：gated 消息区与嵌套
     渲染为 Chat，详情面板为 Detail；Detail 维持现有源码回退与 tail 语义。
  4. 终稿失败保持清帧（不得把预览提升为终稿），消息区显示短原因；
     `Frame`+notice 时原因行照常追加。
  5. i18n：新增 `mermaid-generating`；改写 `mermaid-unclosed`、
     `mermaid-unknown-statement`、`mermaid-too-large`、`mermaid-unsupported`
     中「展示源码」的措辞（en 与 zh-CN）。
  6. 文档/注释同步：`docs/code-index/peri-tui.md` Mermaid 段落
     （「不支持/校验失败的图保留原文与简短原因」改为「消息区只显示短原因，
     源码在详情」）、`display.rs` 模块注释、`peri-tui-components/src/markdown/types.rs`
     围栏源码回退说明。
- 明确不做：
  - `peri-tui-components/src/mermaid/frame.rs` 的紧凑排版、`max_width_strict`
    重试与 `render_final` 验收；不得放宽终稿校验。
  - `stream/` 语法识别器的语义扩展（未知语句后继续绘制），除非新增失败样本
    证明确有回归；不新增第二套正则语法。
  - `hscroll::offset_for` 的 clamp/清零语义、几何键与预算。
  - Kitesurf、任何 commit/push。
- 回归测试（先加失败样本，再修实现）：
  - `peri-tui-components/src/mermaid/preview_test.rs`：
    `peek_keeps_the_last_frame_when_source_is_appended`、
    `peek_keeps_the_last_frame_across_width_change`、
    `closed_final_in_flight_peek_keeps_the_preview_frame`、
    `peek_after_session_switch_returns_no_frame`、
    `final_failure_drops_the_preview_frame`、
    `preview_failure_keeps_the_last_good_frame`。
  - `peri-tui/src/kit/mermaid/display_test.rs`：
    `no_trusted_frame_shows_a_generating_line`、
    `streaming_frame_has_no_raw_source_tail`、
    `failed_final_shows_reason_and_view_source`、
    `block_diagram_hides_source_until_detail`；同步修改现断言旧行为的用例
    （`:39-58`、`:62-77`、`:81-94`）。
  - `peri-tui/src/kit/message_area/mermaid_viewport_test.rs`：
    只 `prepare`（peek）不 `warm` 的中间帧不得出现整段源码；
    `resize_reuses_the_previous_frame_until_the_new_one_arrives`；
    高度变化后的阅读锚点沿用现有用例。
  - `peri-tui/src/kit/message_area/mermaid_hscroll_test.rs`：
    `removing_the_tail_keeps_the_bar_row_and_hits`。
  - `peri-tui-components/src/mermaid/{stream_test.rs,frame_test.rs}`：
    中文标签、`<br/>`、点线标签、箭头/引号中间切 chunk；失败样本先红后绿。
  - `peri-tui/src/kit/message_area/render_markdown_layout_test.rs:583-636`：
    更新为新契约（drain 后显示图，首帧不再是源码）。

## 验收标准

1. 流式任意一帧，消息区不出现原始 Mermaid 源码；有帧时无源码尾，无帧时只显示
   「图表生成中」或短原因。
2. 合法终稿的节点、边、标签不丢；非法终稿不清帧冒充成功，显示短原因且详情可看源码。
3. 40/120 列下宽图横滚、条行与命中不回归；resize 期间复用旧帧不闪源码。
4. 目标测试与 `cargo clippy` 通过；至少一条真实 Buffer 断言覆盖
   「chunk 到达帧不画源码」。

## 验证记录

- 实现（worktree `fix/mermaid-tui`，未提交）：
  - `preview.rs`：`peek` 增加前缀回退（源码前缀 + 有帧 → 复用 `final_frame`/`frame`，不带尾部；宽度不同也复用）；`FenceState` 拆出 `view_with(include_tail)` 与共用的 `notice`。终稿失败仍清帧。
  - `display.rs`：新增 `MermaidSurface::{Chat, Detail}`；Chat 在 `Source` 时只输出占位/短原因 + 动作行，`Frame` 不追加 tail；Detail 行为不变。`display_lines_peek` 固定 Chat。
  - `renderer.rs`：gated 与嵌套入口 → Chat，详情 scheduled 入口 → Detail，surface 与 `schedule_mermaid` 分离。
  - i18n：新增 `mermaid-generating`，改写 `mermaid-unclosed`/`mermaid-unknown-statement`/`mermaid-too-large`/`mermaid-unsupported`（去「展示源码」措辞，指向图详情）。
- 先红后绿：
  - `peek_keeps_the_last_frame_when_source_is_appended`、`peek_keeps_the_last_frame_across_width_change`、`closed_final_in_flight_peek_keeps_the_preview_frame` 在修复前均因 `peek` 返回 `Source`（`expected a rendered frame`）失败；修复后通过。
  - `display_test` 的 chat 用例先红（整段源码内联、帧后 `B--` 尾、失败/不支持展开源码），实现 Chat 表面后通过。
  - 流式切分样本发现真实缺陷：`match_labeled_edge` 在 `.-` 与 `.->` 同偏移时选了更短的 `.-`，残留 `>` 被当成节点括号，`用户 -. 点线文字 .-> 系统` 永不提交；改为同偏移取最长闭合串后通过。
- 命令与结果（仓库要求经 `scripts/cargo-rmcp-patched.sh` 注入 `[patch.crates-io]`；直接 `cargo` 会在 `turso_serverless` 的 `wasm-fetch` feature 上解析失败）：
  - `./scripts/cargo-rmcp-patched.sh test -p peri-tui-components --lib -- mermaid` → 83 passed，0 failed。
  - `./scripts/cargo-rmcp-patched.sh test -p peri-tui --lib -- mermaid` → 42 passed，0 failed。
  - `./scripts/cargo-rmcp-patched.sh test -p peri-tui --lib -- markdown_layout_tests` → 24 passed（过滤器 `render_markdown_layout` 不匹配模块名）。
  - `./scripts/cargo-rmcp-patched.sh test -p peri-tui --lib` → 2835 passed，7 ignored，0 failed（28.3s）。
  - `./scripts/cargo-rmcp-patched.sh clippy -p peri-tui-components -p peri-tui --all-targets -- -D warnings` → 通过。
  - `./scripts/cargo-rmcp-patched.sh fmt --all -- --check` → 通过；`git diff --check` → 通过。
- 未验证：真实终端下 40/120 列的目视检查；浏览器/ACL 等无关目标。回归覆盖使用真实 Buffer 渲染行断言（`mermaid_viewport_test`、`mermaid_hscroll_test`）。
