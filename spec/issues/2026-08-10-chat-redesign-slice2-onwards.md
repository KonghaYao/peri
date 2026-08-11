# Chat 消息流 Redesign — Slice 2 起跨切片决策记录（active）

**状态**：Active（Slice 2-5 持续追加；各决策以代码事实为准）
**创建日期**：2026-08-10
**规格**：`docs/design/tui-chat-message-flow-style.md`（§6/§7/§8/§10/§11/§15）
**前置**：`spec/archive-issues/tui-general/2026-08-10-chat-redesign-slice1-data-gates.md`（Closed，5 项数据门）
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

Slice 2-4 已落地（D1 定案、冻结机制定案、空 reasoning hash 契约、高度断点 Row1Only、D5/D6 双轨定案——落地代码引用见正文各节）；Slice 5（G-Diff parser 客户端解析 + G-Perf 报告）待做。保持 Active。

本文件累计记录跨切片定案（D 系列冲突消解 + 落地引用），供后续编码切片引用；
已落地项标注代码位置，代码与文档冲突时以代码为准并回写本文件。

## D1：§6.2「同一 streaming response 只能有一个 entry」= 按 message_id 段拆分（定案，零代码改动）

- **冲突**：§6.2 单 entry 表述 vs 现状按 message_id 拆多段（interleaving 刻意设计）。
- **定案**：保留按 message_id 段拆分；「entry」解释为「不按 chunk 新增 block」
  （现状已满足——`append_text`/`append_reasoning` 仅在 message_id 变化或
  tool/subagent 边界时 `flush_text_segment`，chunk 不产生新 block）。
- **依据**：interleaving 是刻意设计；合并需重写 `sync_cache`（高险低值）。
- **代码**：`acp_types.rs::flush_text_segment`（message_id 变化/边界 flush）。

## 冻结机制定案：assistant 正文时长（§6.2 `12.4s`）镜像 reasoning 冻结

- **模型**：`TuiAssistantBubble` 新增 `started_at: Option<Instant>`（仅 trailing
  流式段有值）与 `duration_ms: Option<u64>`（冻结值）；`CurrentTurn` 新增
  `text_started_at`（首次 `append_text` 记录，`reset()` 清空）。
- **冻结点**：`apply_fold_pass` 在 phase 离开 PromptRunning 的翻转点对持有
  `started_at` 的 assistant bubble（实践中只有 trailing 一个；冻结段在
  `build_bubble_parts` 恒 None）clone + `started_at.take()` 冻结 `duration_ms`
  + `recompute_hash()`。快照在 TurnDone 后静态，冻结值持续。
- **hash 三单点（G1）**：`TuiAssistantBubble::compute_hash(text, reasoning,
  duration_secs)` / `recompute_hash` / `build_bubble_parts` content_hash 公式
  逐项一致，末位追加 `duration_secs`（running=started_at elapsed 秒取整，
  冻结=duration_ms/1000，None→0）。
- **非流式通道**（`handle_committed_assistant_text` turn.rs:289、rewind 重放
  system.rs:126）：不设置 `started_at`/`duration_ms`（G-Tokens 降级，数据不可达）。
- **代码**：`tui_render_unit.rs::TuiAssistantBubble`、`acp_types.rs::build_bubble_parts`、
  `acp_events/render.rs::apply_fold_pass`。

## 空 reasoning 占位（§6.3）hash 契约

- `build_bubble_parts`：`running == true && reasoning.is_empty()` → 产出空文本
  `TuiReasoningBlock`（fold=Preview、status=Running、started_at=推理起点）；
  `running == false` 的空 reasoning 仍返回 `None`（冻结段无占位，避免历史噪音）。
- 空块 hash = 既有公式（`tui_hash_roll("")==0`，公式无需改）——与 `None` 分支
  hash 各异、跨 rebuild 稳定（R6 测试锁定）。
- 渲染 `render_reasoning_block` running 分支：`◐ Thinking…` + elapsed，空 tail
  由既有空行跳过逻辑处理；`is_trailing_answer`/todo 插位无需改
  （`reasoning.is_some()` 成立）。

## 高度断点（§11）定案：Row1Only 高度 2

- `layout_plan(h)` 纯函数（`layout.rs`）：`h≥12` Full（状态栏 4 行）/ `8≤h<12`
  Row1Only（Row1 + NotifRow，2 行；隐藏 Row2 key hints 与缓冲行）+ 隐藏 session
  title / `h<8` Hidden（0 行）+ `composer_max_lines=Some(2)`。
- 40×8 冒烟数学：status 2 + composer 3（editor 1 + border 2）→ transcript = 3 ≥3。
- `StatusBar(hide_hints, hidden)` / `InputArea(max_lines, session_title_visible)`
  props；Row2 是独立组件，条件渲染不违反 hook 顺序。
- 注意：`element!` 子节点以 `{ if ... }` 块开头会与前一元素（如 `StatusBarRow1()`）
  合并为其 children（NoProps 无 children 字段 → 编译错误）；条件子节点应使用
  一等 `if` 控制流（`StatusBarRow1()` 后直接 `if cond { ... }`）。

## 待追加（后续切片）

- G-Diff 落地（`diff_parser.rs`）→ Slice 5
- G-Perf 对比报告（`PERI_RENDER_TIMING=1` 四段耗时对照 Slice 1 基线）→ Slice 5

## D5：interaction 双轨（定案，已落地 Slice 4）

- **冲突**：inline transcript block（§6.8 要求 permission/AskUserQuestion 入
  transcript）vs 全量迁移（击穿 focus 仲裁与既有测试）。
- **定案**：**双轨**——inline transcript block（AskUser + HITL 都建）承担
  「可见 + 可聚焦 + 结果回写」；AskUser 面板 / HITL 弹窗保留为模态操作层。
  两条响应通道统一走 `HITL_RESPONSE_TX` / `ASK_USER_RESPONSE_TX`（消费者
  RPC 成功后发 `InteractionResolved` 本地事件回写 inline block）。
- **代码**：`acp_events/system.rs`（`handle_ask_user` / `handle_hitl_pending`
  创建点 + `handle_interaction_resolved` 回写）、`message_area/mod.rs`
  （`submit_interaction_option` + `pending_interaction_of`）、
  `hitl_response.rs` / `ask_user_action.rs`（`emit_interaction_resolved`）。

## D6：`[Always allow]` 为协议依赖项（定案，已落地 Slice 4）

- **冲突**：§6.8 选项渲染 vs `HitlPending` 无 optionId 列表
  （event_data.rs:90-95，不得发明协议字段）。
- **定案**：HITL 只渲染 `[Allow once] [Deny]` 两选项；`[Always allow]` 记入
  active spec 为协议依赖项——`HitlPending` 协议扩展后再议。
- **代码**：`build_permission_block`（system.rs）options 恒两项；`mod_test`
  / `acp_events_test` 锁定（D6 断言）。

## Slice 4 落地记录

### 4a：模型 + 生产创建点 + 结果回写（§6.8，已落地）

- `TuiAskUserBlock` 扩展：`kind: InteractionKind`（AskUser/Permission）、
  `pending`、`verb`、`question`（人类摘要）、`options`、`result`、
  `request_id: Option<String>`（身份字段不进 hash，同 message_id 先例）；
  `recompute_hash()` 含 kind/pending/verb/question/options/result + fold/
  is_error/user_modified；partial_eq 同步（tui_render_unit.rs:594-674）。
- **生产创建点**：`handle_ask_user`（system.rs:103）与 `handle_hitl_pending`
  （system.rs:87）在开面板/弹窗的同时构造 block **push 到 `state.committed`**
  （不进 CurrentTurn 缓存——sync_cache 段对齐不可破坏），时序即事件到达位置。
- **人类摘要**：Permission = `{verb} wants to run: {input_summary}`
  （`render-interaction-question-permission` FTL，hitl_input_summary 摘要工具
  JSON——pretty 展示保留在弹窗）；AskUser = 首问 header/options labels。
- **结果回写**：响应消费路径（`hitl_response.rs` Approve/Reject、
  `ask_user_action.rs` Submit/Cancel）RPC 成功后经 `LOCAL_EVENT_TX` 发
  `InteractionResolved { request_id, result }`（`emit_interaction_resolved`
  acp_events/mod.rs:457，未初始化静默丢弃幂等）；handler
  （`handle_interaction_resolved` system.rs:122）扫描 committed 中 pending
  block 按 `request_id` 匹配 → clone + `pending=false` + `result` +
  （非 user_modified）折叠为 Completed + `recompute_hash()` + 原位 `set`
  （COW）；不匹配 no-op（防御）。结果文案走双 FTL
  （`render-interaction-result-*`）。
- **折叠表**：`fold_for_status(FoldTarget::Interaction)`——pending→Running→
  Expanded（可聚焦）/ completed→Completed→Expanded（答毕完整展示，用户需求
  2026-08-11 调整；不再自动收束）/ error→Expanded；`FoldKey::Interaction
  (request_id)` 同步进 `fold_key_of` / `apply_fold_override` / 折叠 pass
  （render.rs:349-376，覆盖表优先——用户 Space 手动折叠仍生效）。

### 4b：选项焦点与提交（§6.8/§9/§11，已落地）

- 键盘：entry 焦点落在 pending interaction block 时 Tab/←/→ 循环切换
  option（纯函数 `cycle_interaction_option`，首末回绕——saturating_sub
  首项卡死缺陷已修）；Enter 提交（`submit_interaction_option`：Permission
  0→Approve/else→Reject、AskUser→Submit answers map + 关闭面板/弹窗，
  双轨一致）；Esc 退出焦点单层取消（复用既有模式）。Space 消费但不动作。
  不新增 FocusLayer——仲裁仍走 `focus_router::message_nav_accepts`
  （Tab/←/→ 仅 entry 焦点激活时归属消息区，非 interaction 焦点 Ignored
  放行输入区）。
- 渲染 `render_ask_user_block_lines`（render.rs:1310）：pending 态
  标题 + 问题摘要 + 选项行；Narrow（§11）垂直排列每行一个 `[label]`，
  否则横向拼接（`[Allow once]  [Deny]`，列区间供点击热区）；
  `InteractionLayout`（option_rows/cols）随 slot 缓存重建；视口 post-pass
  对焦点 slot 的当前 option 行应用 selection bg + BOLD（mod.rs:1284-1337，
  首 span 裸空格不变式沿用）；completed 转只读结果行（success/error 符号）。
- 事件注册顺序：interaction option 点击 handler（mod.rs:886）注册在
  scroll handler（:929）之前（同 keepgoing/md 复制/new output 模式）；
  键盘 handler 无冲突面（scroll 只消费 Ctrl+方向键）。

### 4c：Follow 锚定（§8.1/§6.8，已落地）

- 新独立状态 `anchor_slot: Option<usize>`（mod.rs:533，不碰 `follow_bottom`
  二值）；render body 每帧从快照扫描 pending `TuiAskUserBlock` 的 slot
  index（`write_no_update`，同一时刻至多一个 pending block——模态互斥）；
  pending 完成 → 扫描不到 → anchor 自然清除。
- `anchor_visual_range: Option<(usize, usize)>`（mod.rs:691）：每帧由
  anchor_slot 派生（`slot_visual_starts` + slot 内最大 visual_end，resize
  后按新快照/wrap_map 自动重算）。
- `run_auto_follow` 在 `!follow_bottom` 早退（scroll.rs:924）**之前**插入
  anchor 分支（scroll.rs:896）：`anchor_scroll_target` 纯函数判定 block
  末行超出视口 → 视口对齐到 block 底部（浏览态与跟随态均生效——bg
  subagent 并发流式不把 interaction block 推出视口）；block 完成 →
  anchor=None → 恢复原语义，不强制 follow（§15「不抢回 viewport」）。
- 回归：anchor=None 时既有四路径（submit/reset/replay/增长）全绿；
  `SCROLL_PADDING`/`should_follow_after_user_scroll` 未改。

### 测试覆盖（Slice 4）

- `acp_events_test.rs`：block 注入位置（事件到达即 push，含 request_id
  同源断言）、结果回写（pending→只读 + hash 稳定 + 折叠翻转）、mismatch
  no-op、fold 覆盖优先（`test_fold_pass_interaction_pending_expanded_override_priority`）。
- `scroll_test.rs`：`anchor_scroll_target` 矩阵（超出对齐/视口内 noop/
  钳制/None 语义）。
- `mod_test.rs`：fold_key_of/apply_fold_override/pending_interaction_of +
  option 导航矩阵（`test_cycle_interaction_option_wraps_around`，首末回绕）。
- `focus_router_test.rs`：Tab/←/→ 归属矩阵（entry 焦点激活时归属消息区；
  未激活/带修饰符不抢占）。
- `hitl_response_test.rs` / `ask_user_action_test.rs`：RPC 成功后发
  InteractionResolved（双轨共存回归，弹窗/面板既有路径全保绿）。
- `render_test.rs`：pending 态渲染（标题/问题/选项行 + Narrow 垂直排列 +
  completed 单行结果）。

## Slice 3 落地记录

### D4：queued 反转与 cancel 语义（定案，已落地）

- **反转**：`dispatch_submit_request`（input_area.rs）AgentText 分支 `is_loading`
  时**只入队** `INPUT_BUFFER`（保留 32 条上限），不再 `send_local_user_bubble`
  ——排队项不提前进 transcript，显示在 composer 上方队列（§10）。
- **drain（D4）**：`drain_input_buffer`（acp_events/render.rs）对每条排队文本
  先 `send_local_user_bubble(text)`（从 input_area.rs 提出为 pub(crate)）再
  `tx.send(AgentText)`——镜像非 loading 路径，气泡恰出现一次，不依赖服务端回显。
- **cancel 语义补齐**：`handle_turn_interrupted` 非 stale 两分支（零产出回滚 /
  归档）由 `INPUT_BUFFER.clear()` 改为 `drain_input_buffer()`——排队项是用户已
  提交的请求（composer 队列可见），取消不吞排队项，与 stale 分支行为对齐；
  loading 提交不再设置 `last_submitted_text`（不触发 LocalUserBubble），零产出
  回滚不会误恢复排队输入。`handle_local_user_bubble`（非 loading 路径）不改。
- **composer 上方队列渲染**：订阅 `INPUT_BUFFER`，每行 `· {text}`（sym().queued
  + muted，truncate_by_width 截断），最多 5 条 + `· · ·` 溢出行；队列是 composer
  邻接区，不进 transcript/不参与滚动模型；高度计入 InputArea total_height。
- **测试**：`acp_events_test.rs` cancel 用例群（零产出回滚 / 归档 / request_id
  配对 / drain 幂等）断言从"清空排队输入"改为"drain 提交"；新增
  `test_drain_input_buffer_sends_local_user_bubble_once`（FIFO + 气泡恰一次）；
  `input_area_test.rs` dispatch 矩阵（loading 只入队不气泡、非 loading 双发、
  排队上限 32 队首挤出）。

### Slice 3a 对齐适配：计划与代码事实冲突（prompt 前缀宽度方向）

- **计划**：Narrow 时 prompt 前缀缩为 `"❯ "`（2 列）对齐（假设 composer 有
  左右 border，border1 + `" ❯ "`3 = 4 列 vs transcript 3 列）。
- **代码事实**：`build_composer_block` 用 `Borders::TOP|BOTTOM`——**无左右
  border**，composer 正文起点 = prompt 前缀宽度本身。transcript content 起点 =
  `first_prefix_width` = outer1 + accent1 + gap：gap=1（Compact/Narrow）→ 3 列
  与 `" ❯ "` 3 列**已对齐**；gap=2（Wide/Standard）→ 4 列 vs 3 列**错位 1 列**。
- **定案（最小正确适配）**：prompt 前缀宽度 = `2 + gap`（`" ❯"` + gap 空格），
  续行前缀同宽；`prompt_and_border_width(grid)` = 前缀 + 右预留 2（gap=1 → 5，
  gap=2 → 6）。鼠标点击定位 `text_x` 与光标上下视觉移动宽度同步该公式。
  40/60/120 对齐矩阵测试锁定（`test_prompt_prefix_aligns_with_grid_content_start`）。

### Slice 3a：composer 标题/footer（§10，已落地）

- `build_composer_block` 的 title_top 仅保留右侧 session title；mode/model 已由状态栏
  持续显示，不在 composer 左上角重复。title_bottom 左侧 `@ N files`
  （PENDING_ATTACHMENTS 由 OnceLock<Handle> 改 AtomStatic 以订阅唤醒）+ 右侧
  `{pct}% ctx`（CONTEXT_USAGE，status_bar 同源）。
- **逐级隐藏**：h<12（`session_title_visible=false`）隐藏 title_top 整行；
  h<8（`max_lines=Some(2)`）再隐藏 title_bottom。
- FTL 键：`composer-attachments` / `composer-context-usage`（en + zh-CN，
  LANG_VERSION 订阅已覆盖）。

### Slice 3c：interjection 来源字段占位（G-Interjection，已落地）

- `TuiUserBubble` 新增 `source: Option<String>`——协议无来源标记，`new` 与
  `send_local_user_bubble` 恒 None（行为不变）；身份字段不进 content_hash
  （同 message_id 先例），进 partial_eq。
- 渲染 `render_user_bubble_lines`：`source.is_some()` 时 label 追加
  ` · {source}`（muted）——当前恒不触发，纯预留。
- 测试：`tui_render_unit_test` source partial_eq + new 占位；`render_test`
  `test_user_bubble_source_appended_to_label_when_some`。

## Slice 2 落地记录

### D2：group 失败数 = 组后连续相邻 error 计数（定案，已落地）

- `TuiCollapsedGroup` 新增 `failed_count: u32`；`recompute_hash` 纳入
  （`combine(h, failed_count)`）、`tui_impl_partial_eq` 同步
  （`tui_render_unit.rs::TuiCollapsedGroup`）。
- `group_successful_tools`（acp_events/render.rs）：run 结束位置起向后扫描
  **连续相邻** error `TuiToolCard` 计入 `failed_count`；error 不入组、不删除、
  保持展开（§7 表 error→Expanded + §15「error 永不隐藏」）。扫描在删除 run
  元素之前进行（items 索引仍指向原位置）。
- 渲染 `render_collapsed_group_lines`（message_area/render.rs）：标题追加
  `· N failed`（仅 >0，`sem.status.error` 色 span）；title 截断优先于失败后缀
  （失败数不可被截断吞掉），窄屏失败数仍可见。
- 测试：`test_snapshot_group_successful_tools` 扩展 + `render_test`
  `test_collapsed_group_line_with_failed_count` + `tui_render_unit_test`
  `test_collapsed_group_hash_includes_failed_count`。

### 2a：`↓ New output` 指示器（§8.1，已落地）

- 判定纯函数 `scroll::new_output_indicator_active(follow, scroll_y, vis_height,
  content_bottom)`：浏览态（!follow_bottom）且视口未到**真实内容底**（core +
  footer 视觉行数，**不含** SCROLL_PADDING 缓冲——缓冲行不可见，滚到视觉底部
  即消失，与 `should_follow_after_user_scroll` 扣缓冲口径对齐）。
- 渲染（mod.rs render body）：`viewport_lines` 末尾（有 footer 时插在 footer
  之前）插入指示行 `↓ {msg-new-output}`（`sem.status.running` + BOLD；unicode
  降级 `v`）；视口附加行，不进 VmCacheSlot / wrap_map / total_visual_rows
  （G3 视口级），NO_COLOR 剥离 pass 天然覆盖。
- 点击热区仿 CopyButtonHit 模式：render body 存屏幕 rect（`write_no_update`），
  handler 注册在 scroll handler 之前；点击 → `follow=true` + `scroll_to_bottom()`
  （与 End 键同路径）。键盘 End 已有，不改。
- 测试：`scroll_test.rs` 指示器矩阵（浏览/跟随/到底/边界/缓冲口径）。

### 2c：subagent Enter 详情 pane（§6.7，已落地）

- 新 atom `SELECTED_SUBAGENT_ID: AtomStatic<Option<String>>`（atoms.rs）。
- `PanelKind::SubAgentDetail` 注册入 panel_registry（PANELS 表 +
  panel_title/panel_description 分派 + ALL_PANEL_KINDS 矩阵）；与 Agent 面板
  同 `MutexGroup::Agent`（打开详情关闭 Agent 面板）；无快捷键/slash 命令
  （仅 Enter 分派打开）。
- 新面板 `panels/subagent_detail.rs`：从 VIEW_MODELS 扫描 `TuiSubAgentGroup`
  （含 TuiCollapsedGroup 内层与嵌套 SubAgent 递归，与 agent.rs collect_subagents
  同口径），用 `GridSpec::with_content` 嵌套渲染，复用
  `vm_to_lines_cached(..., render_copy_button=false)`（可见性提升为 pub(crate)）；
  `register_panel_scroll` 可滚动；Esc 单层关闭（`close_active_panel` 弹栈）。
- 焦点分派（mod.rs Enter 分支）：焦点在 `TuiSubAgentGroup` 上时 Enter **不切
  折叠**（subagent 折叠恒 Collapsed 是 §7 表裁决，`fold_key_of`/
  `apply_fold_override` 不动），改 `open_panel(SubAgentDetail)` + 写
  `SELECTED_SUBAGENT_ID`；Tool/Reasoning 的 Enter 语义不变。Space 仍切折叠。
- 新 FTL 键：`panel-title-subagent-detail` / `panel-desc-subagent-detail` /
  `subagent-detail-not-found`（en + zh-CN，`LANG_VERSION` 订阅已覆盖）。
- 测试：`subagent_detail_test.rs`（find_selected_subagent 矩阵）、
  `panel_registry_test.rs`（注册/互斥/弹栈）、`mod_test.rs`（subagent fold_key
  锚定）。

### 2d：鼠标单击展开（§9.1，已落地）

- **缺口**：消息区内 `Down(Left)` 被 scroll handler 记为选区锚点、无拖拽 `Up`
  什么都不做——单击无任何展开/详情入口。
- **设计决策**（用户确认）：命中区域 = entry 逻辑首行（header/label 行）；
  语义与键盘 Enter 完全一致（toggle + subagent 详情面板）；点击同时设置
  entry 焦点。
- **实现**：
  - `scroll.rs` 新增纯函数：`is_click`（Down/Up 坐标差 ≤1 行、≤2 列，手抖
    容差；无 Drag 已表明未移动，容差防事件丢失）、`entry_click_target`
    （wrap_map 反查 + slot_offsets 换算，仅 `local_idx == 0` 命中；header
    wrap 成多视觉行全部命中；footer 区域 None）；`is_scrollbar_column`
    提升 `pub(super)`。
  - `mod.rs` 新增「entry 单击展开」handler：处理 `Up(Left)`，注册在
    scroll handler 之前（keepgoing / md 复制 / interaction option 之后）。
    放行条件：occluded、坐标外、滚动条列、`dragging`（选区复制）、无 Down
    锚点、超容差、非首行。命中 → `entry_focus` + `FOCUSED_ENTRY_KEY`
    （foldable 才有值）+ 重置 interaction option + 清残留选区 +
    `apply_fold_toggle(snapshot, slot, false)`（subagent → 详情面板，
    pending interaction 首行仅聚焦不提交，无折叠能力 entry 仅聚焦）。
  - `entry_focus` use_state 定义前移到点击 handler 之前（hook 顺序每次
    渲染一致，TUI-HOOK-001 满足）。
- **测试**：`scroll_test.rs` 容差矩阵 / 首行命中（含 wrap 续行、多 slot 偏移、
  footer、空 map）；`mod_test.rs` 动作层（subagent → SELECTED_SUBAGENT_ID +
  面板、tool → fold toggle + FOLD_OVERRIDES）。
