# History V2 验收与发布说明

> 状态：本地 SQLite + TUI 的唯一历史扩展验收记录。
>
> 依据：仓库外设计稿《Peri ACP 统一历史加载协议详细设计》v2.0（2026-10-22）§2、§9、§12、§13、§15；
> 本文只记录可复现证据与明确缺口，**不把未验证项写成通过**。验收编号沿用设计稿 A01–A44。

## 1. 范围

1. 覆盖**本地 SQLite 后端 + TUI**：History V2 是唯一历史路径，`history-window-v1` 与旧私有
   分页/窗口协议已删除；`history-v2` 仅作为无行为迁移键保留。
2. 标准 ACP 兼容边界保留：未声明 `_meta["peri.history"]` 的客户端仍得到标准
   `session/load` 完整回放；Peri TUI 不进入该回退。
3. 远端（remote）与多实例仍显式失败：`supports_history_v2()` 默认 false，协商阶段返回
   `unsupported_version`；本轮不验收跨实例失效、接管或远端 V2。
4. 不包含：数据库表/列变更、旧协议重新启用、持久执行恢复、远端性能基准。

## 2. 验收状态（A01–A44）

状态口径：

- **已覆盖**：核心通过标准有直接回归测试。
- **部分覆盖**：只有分层/局部证据，设计要求的规模、并发或端到端场景缺失。
- **未覆盖**：无实现或无回归证据。
- **范围外**：本轮明确不验收，且不会标为通过。

| ID | 状态 | 证据 / 缺口 |
| --- | --- | --- |
| A01 | 部分覆盖 | `history_v2_adapter_test::open_latest_reports_tail_cursors_and_ordered_entries`、`history_service/service_test::oversized_page_is_budget_exceeded_and_keeps_window`；10 万条会话与 active 恢复成本未实测（`session_history_perf_*` 仍为 `#[ignore]` 或独立压测入口）。 |
| A02 | 部分覆盖 | `history_v2_adapter_body_test::oversized_payload_above_limit_returns_budget_exceeded_with_bounded_reads` 证明 >8 MiB 单条正文不整读；100 MiB 真实工具输出仅以 opaque descriptor 出现在首屏的场景未构造。 |
| A03 | 部分覆盖 | 一条 canonical payload 的子项全量投影进同一 `entryId`（`history_v2_adapter/view.rs`），响应总字节由 `budget_exceeded` 兜底；子项级正规化目录与子项级分页未实现。 |
| A04 | 部分覆盖 | `history_v2_adapter_test::system_and_model_only_entries_are_not_projected`、`history_v2_adapter_visibility_test::explicitly_hidden_reminder_is_still_skipped`；5000 条不可见消息下空页游标严格前进未单测。 |
| A05 | 已覆盖 | `history_service/service_test::{same_key_same_fingerprint_replays_success,same_key_different_fingerprint_is_request_conflict}`、`initial_test::initial_replays_same_bytes_and_rejects_conflicting_params`、`history_v2_load_test::session_load_v2_same_request_key_replays_initial_bytes`。 |
| A06 | 部分覆盖 | `history_service/body_test::close_session_closes_only_that_sessions_windows`、`history_v2_load_test::session_close_invalidates_v2_windows`；同一会话两条真实连接的窗口隔离未建立。 |
| A07 | 部分覆盖 | 首屏字节重放见 A05；`handle_load_shared` 仍先执行装配，未断言「同键重试不重复 runtime/MCP 装配」。 |
| A08 | 已覆盖 | `initial_test::{initial_backend_failure_reports_effect_and_leaks_no_window,initial_failure_without_execution_ready_hides_runtime_generation}`、`history_v2_load_test::session_load_v2_initial_failure_returns_effect_without_execution`。 |
| A09 | 已覆盖 | 服务端：`live_delivery_test::paused_updates_are_buffered_until_resume`；TUI：`live_test::{admission_accepts_only_contiguous_transcript_sequence,pending_buffer_dedupes_sorts_and_drops_oldest_on_overflow}`、`history_v2_load_test::session_load_v2_live_updates_follow_boundary_and_reload_replaces_stream`。 |
| A10 | 已覆盖 | `live_delivery_test::{count_overflow_drops_oldest_and_exposes_gap,byte_overflow_keeps_buffer_bounded}`、`history_v2_view_test::detached_keeps_the_accepted_prefix_and_shows_a_gap_warning`。 |
| A11 | 已覆盖 | `server_loop_history_test::{server_loop_history_cancel_is_processed_while_body_blocked,server_loop_history_close_returns_before_blocked_page_result}`：真实 ServerLoop 中慢读不阻塞 cancel/close。执行锁并发场景仍未单测。 |
| A12 | 未覆盖 | V2 initial 在真实执行进行中的准入等待/拒绝路径无回归；旧执行准入测试已随协议删除。 |
| A13 | 已覆盖 | `paging_test::{late_response_from_old_request_does_not_touch_new_state,response_from_an_old_window_is_discarded_and_resets_in_flight}`、`consumer_test::{late_failure_from_previous_session_does_not_touch_new_window,late_open_failure_keeps_the_new_session_claim}`。 |
| A14 | 已覆盖 | `paging_test::seek_preempts_page_and_old_page_cannot_clear_the_new_request`、`body_test::late_body_response_does_not_write_into_a_newer_request`、`history_v2_view_test::same_epoch_page_keeps_detail_but_seek_closes_it`。 |
| A15 | 部分覆盖 | `paging_test::{anchor_becomes_unavailable_after_its_entry_is_evicted,coverage_gap_is_explicit_and_never_treated_as_continuous}`、`history_v2_view_test::{seek_keeps_live_tail_and_marks_it_as_live,seek_dedupes_live_message_already_in_the_new_page,reopen_keeps_streaming_content_not_in_the_new_page}`；终端 resize 下的视觉锚点未自动化。 |
| A16 | 部分覆盖 | `history_v2_adapter_test::tool_call_and_result_share_invocation_key_across_entries`、`history_v2_view_test::{tool_call_and_result_merge_across_entries,tool_result_without_call_is_a_standalone_card,different_invocation_keys_do_not_merge}`；跨 source 同名 ID 未单测。 |
| A17 | 已覆盖 | `history_v2_adapter_test::append_stays_outside_the_fixed_tail_and_tail_removal_invalidates`、`service_test::{backend_revision_change_invalidates_and_removes_window,backend_committed_tail_change_invalidates_and_removes_window}`。 |
| A18 | 部分覆盖 | `service_test::{backend_revision_change_invalidates_and_removes_window,close_during_page_discards_stale_page}`；V2 下 rewind/删除后旧窗口失效的 Host 端到端未单测（`requests_rewind_*` 只覆盖 transcript 与执行视图）。 |
| A19 | 范围外 | 适配器明确 `historyRevision` 是 own 存储序 tail position，不是持久结构版本；跨实例失效与第二进程改写不在本轮承诺内。 |
| A20 | 部分覆盖 | fixed tail 与 tail 变化失效见 A17；V2 不消费 compact 提交通知，compact 后旧窗口按 revision/tail 复核失败并重开的 e2e 未建立。 |
| A21 | 部分覆盖 | `history_v2_adapter_inherited_test::{open_latest_orders_inherited_before_own_and_keeps_own_tail,before_pages_cross_regions_without_duplicates,after_page_from_inherited_anchor_continues_into_own_without_skipping,seek_inherited_returns_anchor_once_with_stable_ordinals,inherited_oversized_visibility_uses_bounded_prefix,own_append_does_not_change_the_frozen_prefix_and_tail_delete_invalidates,oversized_inherited_without_index_maps_to_index_not_ready}`；父会话删除场景未单测。 |
| A22 | 部分覆盖 | `history_v2_adapter_inherited_test::oversized_inherited_without_index_maps_to_index_not_ready`、`initial_test::initial_without_execution_ready_still_returns_page`、TUI `initial_test::execution_not_ready_keeps_history_readable_but_not_executable`；只读 SQLite 与执行目录不存在未构造。 |
| A23 | 已覆盖 | `service_test::{page_wrong_ticket_is_ticket_invalid,page_wrong_snapshot_token_is_snapshot_invalidated,page_session_mismatch_is_permission_denied}`、`history_service/cursor_test`（MAC/窗口/旧 cursor）、`body_test::body_ref_must_be_issued_by_the_window`。 |
| A24 | 已覆盖 | `service_test::{close_then_page_is_window_closed_and_close_replays,close_during_page_discards_stale_page}`、`body_test::body_returns_chunk_and_close_rejects_later_body`、`history_v2_end_to_end_test` 的 close 阶段。 |
| A25 | 已覆盖 | `service_test::{idle_window_expires_after_five_minutes,absolute_window_expiry_wins_over_recent_use,opening_placeholder_is_pruned_after_sixty_seconds,capacity_rejects_ninth_window_and_expiry_frees_a_slot}`。 |
| A26 | 部分覆盖 | TUI 保留预算与失效：`paging_test::{retention_bounds_retained_items_and_keeps_reload_cursor,anchor_becomes_unavailable_after_its_entry_is_evicted}`、`body_test::body_budget_rejects_before_allocating_unbounded_state`；长时间滚动 RSS 与 pin 阻塞可见性未实测。 |
| A27 | 已覆盖 | TUI `body_test::{body_chunks_concatenate_in_offset_order_and_stop_when_complete,late_body_response_does_not_write_into_a_newer_request}`、`history_detail_test::{detail_chunks_cross_utf8_boundary_without_garble,detail_incomplete_tail_at_eof_is_still_corruption}`、`history_v2_adapter_body_test::oversized_tool_call_and_result_bodies_render_semantic_text`。 |
| A28 | 已覆盖 | 标准连接：`history_v2_load_test::session_load_v2_legacy_response_is_unchanged_and_missing_meta_is_rejected` 证明未声明 `peri.history` 时仍走标准回放；`requests/session_caps.rs` 内联测试覆盖无声明不产生 V2 cap。 |
| A29 | 已覆盖 | `requests/session_caps.rs::{legacy_only_history_declarations_do_not_select_or_echo_v2,mixed_legacy_and_v2_declarations_are_rejected}`、`peri_caps_test::{test_retired_history_capability_keys_are_not_parsed_or_echoed,test_history_window_v1_is_retired}`、`history_v2_load_test::session_load_v2_mixed_fields_and_unnegotiated_initial_are_rejected`、TUI `requests_caps_test::{test_history_v2_declaration_requires_explicit_server_selection,test_legacy_history_capabilities_never_satisfy_startup}`。 |
| A30 | 部分覆盖 | 本地真实 SQLite 全链路：`history_v2_end_to_end_test`、`history_v2_adapter_test::local_sqlite_advertises_history_v2_and_unknown_sessions_fail_closed`；remote 未实现 V2（fail-closed），远端读取量与往返为范围外。 |
| A31 | 已覆盖 | `requests_caps_test::test_history_v2_declaration_requires_explicit_server_selection`：服务端未选择 V2 时 TUI 启动失败；`session_caps::negotiate_history_v2` 不会把旧能力或缺失声明升级成 V2。 |
| A32 | 部分覆盖 | `initial_test::initial_replays_same_bytes_and_rejects_conflicting_params` 覆盖同态重放与 `execution_ready`/`runtimeGeneration`；runtime 已关闭后的重试未单测。 |
| A33 | 部分覆盖 | `initial_test::{initial_delivery_expires_after_ledger_eviction,initial_delivery_expires_with_its_window}`、`history_v2_load_test::session_load_v2_closed_window_replay_is_delivery_expired`；客户端暂存丢失但服务端仍可重放的场景未单测。 |
| A34 | 已覆盖 | TUI `consumer_test::{window_expiry_requires_reopen_and_reopen_enqueues_open,snapshot_invalidated_resets_claim_and_reopen_replaces_the_window,seek_advances_epoch_and_replaces_retained_window,open_latest_sends_open_then_closes_old_window_only_after_admission,wire_requests_follow_enqueue_order_across_all_v2_rpcs}`、`history_v2_view_test::{window_expiry_renders_warning_note_and_keeps_retained_pages,full_flow_pages_seeks_reads_body_and_survives_window_expiry}`。 |
| A35 | 未覆盖 | `_peri/history/changed` 只在协商中回显名称，服务端未发送 committed/invalidated/closed；无 itemSeq 封口证明。客户端当前依赖请求错误与重新 `open`。 |
| A36 | 已覆盖（范围调整） | 本轮范围调整为复用现有 schema：未新增表/列；有界读取与 descriptor 证据见 `history_v2_adapter_test`、`history_v2_adapter_body_test`、`history_service/service_test::oversized_page_is_budget_exceeded_and_keeps_window`。 |
| A37 | 部分覆盖 | `service_test::oversized_page_is_budget_exceeded_and_keeps_window`、类型层 `history_v2_test::progress_budget_exhaustion_requires_progress`；超小预算、元数据超大、成功空页无进展未专门压测。 |
| A38 | 已覆盖 | `service_test::{ledger_byte_budget_evicts_success_and_retry_expires,gap_then_old_key_is_operation_expired}`、`initial_test::initial_delivery_expires_after_ledger_eviction`。 |
| A39 | 部分覆盖 | `history_service/cancel_test::oversized_params_are_invalid_before_decode_and_state_registration`（64 KiB 入站上限）与类型层未知字段/范围校验；深嵌套、4096 元素、并发洪峰未测。 |
| A40 | 部分覆盖 | `history_v2_adapter_test::oversized_entry_is_opaque_and_body_returns_projected_text`、`service_test::{body_invalid_offset_and_zero_max_are_invalid_params,body_over_limit_is_invalid_params}`、类型层 `history_common_test::body_chunk_validation_requires_progress_and_bounded_bytes`；服务端空未完成块未单测。 |
| A41 | 已覆盖 | `service_test::{concurrent_same_key_returns_operation_in_progress,in_progress_can_be_retried_after_120_seconds}`、`cancel_test::{cancelled_open_releases_placeholder_and_allows_immediate_retry,request_timeout_releases_placeholder_and_allows_immediate_retry}`。 |
| A42 | 部分覆盖 | own 只发布 durable 行并带 `historyRevision`/`committedTail`（`history_v2_adapter_test::open_latest_reports_tail_cursors_and_ordered_entries`）；客户端封口证明、迟到 delta 与 partial 行协议未实现。 |
| A43 | 部分覆盖 | fixed tail 与 append 后新 `open` 可见（A17 同源测试）；显式 durable pin / GC 协调未实现。 |
| A44 | 已覆盖 | `initial_test::initial_replaces_same_session_windows_and_leaves_others_live`、`history_service/initial_replacement_test::{initial_success_switches_live_only_after_commit,initial_replacement_at_capacity_is_bounded_across_success_failure_and_replay}`、`history_v2_load_test::session_load_v2_repeated_loads_do_not_exhaust_window_capacity`、`history_v2_end_to_end_test::history_v2_end_to_end_repeated_loads_replace_window_without_capacity_error`、`live_delivery_test::{close_removes_stream_and_later_updates_pass_through,close_if_paused_never_tears_down_an_active_stream}`。 |

**汇总：已覆盖 19 项 + 范围调整后已覆盖 1 项（A36），部分覆盖 21 项，未覆盖 2 项（A12、A35），范围外 1 项（A19）。**

未覆盖与部分覆盖项不能表述为 V2 已满足完整设计验收。主要缺口：

1. A03：子项级正规化目录未实现，单条 canonical 含海量子项时只能整条返回或 `budget_exceeded`。
2. A12 / A35：执行中的 initial `busy`、`_peri/history/changed` 与封口证明未实现。
3. A18 / A20 / A30：rewind/compact 的 V2 端到端、远端合同未覆盖。
4. A01 / A02 / A26 / A37：规模、内存与压测证据仍不足。

## 2.1 本轮验证结果

命令：`./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib`。以下为交付前全量
lib 测试记录，五个 crate 均 0 failed；本次文档修改未重跑测试。

| Crate | 通过 | 忽略 | 结果 |
| --- | --- | --- | --- |
| `peri-acp-types` | 691 | 未单列 | 通过 |
| `peri-config` | 160 | 未单列 | 通过 |
| `peri-resources` | 581 | 32 | 通过 |
| `peri-tui` | 2214 | 6 | 通过 |
| `peri-acp` | 1151 | 未单列 | 通过 |

## 3. 本轮修复证据（FIX-01～05、CLEAN-01）

证据来自当前工作区的测试源码与 §2.1 的交付前全量结果。覆盖边界按源码与用例名核对；
crate 级计数不替代单项场景证明，未覆盖部分不记为通过。

| 任务 | 实现位置 | 回归测试 | 覆盖边界 |
| --- | --- | --- | --- |
| FIX-01 | `peri-tui/src/kit/history_v2/outbound.rs`、`consumer.rs` | `consumer_test::wire_requests_follow_enqueue_order_across_all_v2_rpcs`、`consumer_test::open_latest_sends_open_then_closes_old_window_only_after_admission`、`consumer_test::close_request_uses_window_identity_without_faking_closed_state` | 覆盖 initial→Close→page→body→open→Close→seek 的入队/上线顺序与 requestKey 单调；不覆盖真实 socket/多连接重排。 |
| FIX-02 | `peri-acp/src/host/history_v2_adapter/{body,view}.rs`、`peri-resources/src/sessions/sqlite_store/session_data/history_window.rs` | `history_v2_adapter_body_test::{oversized_tool_call_and_result_bodies_render_semantic_text,oversized_payload_above_limit_returns_budget_exceeded_with_bounded_reads}`、`history_v2_adapter_budget_test::{around_seek_after_entry_is_opaque_and_body_reads_fully,inherited_around_seek_after_entry_is_opaque_and_body_reads_fully,cross_region_open_keeps_inherited_oversized_body_readable,same_entry_inline_and_opaque_yields_identical_display_text}`、`history_v2_adapter_inherited_test::inherited_body_chunks_match_projected_text` | 覆盖真实 SQLite 的 own/inherited 正文、独立元数据定位与 descriptor/body 表示一致；不覆盖存储瞬态错误下的重试时序。 |
| FIX-03 | `peri-acp/src/host/history_v2_adapter/{projection,view}.rs` | `history_v2_adapter_visibility_test::{truncated_multibyte_prefix_keeps_reminder_visible,unknown_role_and_inherited_use_the_same_prefix_rules,undetermined_prefix_fails_the_page_and_retry_reads_the_entry,injected_prefix_read_failure_fails_page_then_retry_reads,explicitly_hidden_reminder_is_still_skipped}` | 覆盖 1024 字节前缀的结构化 JSON 判定、UTF-8 截断、无法判定/读取失败整页失败；未覆盖真实磁盘 I/O 抖动。 |
| FIX-04 | `peri-acp/src/host/server_loop/history.rs`、`history_service/operations.rs`、`live_delivery.rs` | `server_loop_history_test::{server_loop_history_cancel_is_processed_while_body_blocked,server_loop_history_close_returns_before_blocked_page_result,server_loop_history_disconnect_drops_blocked_page_without_publishing,server_loop_history_disconnect_drops_blocked_body_without_publishing,server_loop_history_capacity_saturation_rejects_without_waiting}`、`live_delivery_test::{candidate_keeps_old_stream_identity_and_buffers_independently,commit_candidate_replaces_stream_and_flushes_candidate_buffer,close_discards_candidate_and_old_stream}` | 覆盖同步准入、在途配额 4、close 快速路径、取消/断连迟到结果不发布；真实 transport socket 断连未做端到端用例（本地 ServerLoop 断连治理见 T10），`session/load` 仍在循环内 `await`。 |
| FIX-05 | `peri-acp/src/host/history_service/{initial,window}.rs`、`live_delivery.rs` | `history_service/initial_replacement_test::{initial_backend_failure_keeps_old_window_and_live_identity,initial_budget_failure_keeps_old_window_and_live_stream,cancelled_initial_keeps_old_window_and_live_stream,close_session_during_initial_prepare_does_not_revive_old_window,initial_success_switches_live_only_after_commit,initial_replacement_at_capacity_is_bounded_across_success_failure_and_replay}` | 覆盖准备失败/预算失败/取消/会话关闭时旧窗口与旧 streamId/afterSeq 保留，以及 max+1 替换边界；不覆盖跨连接与进程崩溃。 |
| CLEAN-01 | `history_common`、`session_resources::history_window`、`history_detail_binding`、`history_page` 删除项 | `history_v2_load_test::{session_history_body_is_method_not_found,session_load_v2_mixed_fields_and_unnegotiated_initial_are_rejected,session_load_v2_legacy_response_is_unchanged_and_missing_meta_is_rejected}`、`session_io_test::removed_execution_protocols_are_unsupported`、`requests/session_caps.rs::{legacy_only_history_declarations_do_not_select_or_echo_v2,mixed_legacy_and_v2_declarations_are_rejected}`、`peri_caps_test::{test_retired_history_capability_keys_are_not_parsed_or_echoed,test_history_window_v1_is_retired}`、`client_test::test_pump_has_no_typed_path_for_legacy_history_committed` | 覆盖旧能力不回显、旧顶层字段拒绝、旧方法 method not found、TUI 无旧通知解析路径；以静态不存在与负向断言为主，不做运行时模糊测试。 |

## 4. 本地验收矩阵（T01–T18）

| 编号 | 场景 | 证据 | 状态 |
| --- | --- | --- | --- |
| T01 | 加载已有会话 | `history_v2_end_to_end_test::history_v2_end_to_end_initial_pages_seek_body_live_and_close` | 已覆盖 |
| T02 | 上翻、下翻、定位 | `history_v2_adapter_test::{page_walks_before_and_after_without_duplicates,seek_places_the_anchor_once_with_both_neighbors}`、TUI `paging_test` | 已覆盖 |
| T03 | 重开与窗口过期恢复 | `consumer_test::{window_expiry_requires_reopen_and_reopen_enqueues_open,snapshot_invalidated_resets_claim_and_reopen_replaces_the_window}`、`history_v2_end_to_end_test::history_v2_end_to_end_repeated_loads_replace_window_without_capacity_error`（重复 load 后新窗口 page/body 可用、旧窗口 `window_closed`） | 已覆盖 |
| T04 | body 与其他动作并发发起 | `consumer_test::wire_requests_follow_enqueue_order_across_all_v2_rpcs` | 已覆盖（单进程 mpsc） |
| T05 | own around seek 剩余预算不足 | `history_v2_adapter_budget_test::around_seek_after_entry_is_opaque_and_body_reads_fully` | 已覆盖 |
| T06 | own/inherited 拼页 | `history_v2_adapter_budget_test::{cross_region_open_keeps_inherited_oversized_body_readable,same_entry_inline_and_opaque_yields_identical_display_text}` | 已覆盖 |
| T07 | 中文/emoji 前缀截断 | `history_v2_adapter_visibility_test::truncated_multibyte_prefix_keeps_reminder_visible` | 已覆盖 |
| T08 | 可见性读取失败与隐藏条目 | `history_v2_adapter_visibility_test::{undetermined_prefix_fails_the_page_and_retry_reads_the_entry,injected_prefix_read_failure_fails_page_then_retry_reads,explicitly_hidden_reminder_is_still_skipped}` | 已覆盖 |
| T09 | 慢 page/body 期间 cancel | `server_loop_history_test::server_loop_history_cancel_is_processed_while_body_blocked` | 已覆盖 |
| T10 | 慢读取期间 close/断连 | `server_loop_history_test::{server_loop_history_close_returns_before_blocked_page_result,server_loop_history_disconnect_drops_blocked_page_without_publishing,server_loop_history_disconnect_drops_blocked_body_without_publishing}`；真实 transport 断开复用同一 cancellation token，未做端到端 socket 用例 | 已覆盖（本地治理路径） |
| T11 | 新 initial/open 准备失败 | `history_service/initial_replacement_test` 六例；TUI `acp_client/client/history_v2_test.rs::{failed_initial_keeps_the_previous_window_and_delivery_identity,failed_initial_reprojects_the_previous_same_session_view}`；跨会话失败按设计不恢复旧视图 | 已覆盖 |
| T12 | 配额边界连续重开/失败重试 | `initial_replacement_at_capacity_is_bounded_across_success_failure_and_replay`、`server_loop_history_capacity_saturation_rejects_without_waiting` | 已覆盖 |
| T13 | 初始加载与实时通知交错 | `history_v2_load_test::session_load_v2_live_updates_follow_boundary_and_reload_replaces_stream`、`live_delivery_test::candidate_keeps_old_stream_identity_and_buffers_independently` | 已覆盖 |
| T14 | local rewind、删除后继续读旧窗 | 服务端 `session_close_invalidates_v2_windows`；rewind 的 V2 端到端缺失 | 部分覆盖 |
| T15 | 旧私有协议退出 | CLEAN-01 行所列负向测试 | 已覆盖 |
| T16 | 标准 ACP 兼容边界 | `history_v2_load_test::session_load_v2_legacy_response_is_unchanged_and_missing_meta_is_rejected` | 已覆盖 |
| T17 | 长历史读取预算 | adapter 有界读取测试；未记录数据规模/读取量/耗时 | 部分覆盖 |
| T18 | 详情、滚动位置和实时尾部 | `history_v2_view_test::{seek_keeps_live_tail_and_marks_it_as_live,seek_dedupes_live_message_already_in_the_new_page,reopen_keeps_streaming_content_not_in_the_new_page}`、`history_detail_test` | 已覆盖 |

## 5. 静态核对与可复现命令

本轮完成的静态核对：

1. 对已删除的旧能力键、旧通知/方法与旧分页字段做全仓检索（`docs` 与两个模块 `CLAUDE.md`）：剩余命中只出现在删除/拒绝/迁移说明。
2. 本文引用的测试文件与用例名均以当前工作区 `rg -n 'fn [a-z_]+'` 核对存在。
3. 本次文档修改未重跑 Cargo；测试结果以 §2.1 记录的交付前全量运行为准，下列命令保留为复核入口。

```bash
./scripts/cargo-rmcp-patched.sh check --locked \
  -p peri-acp-types -p peri-resources -p peri-acp -p peri-tui -p peri-config
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib -- history
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history
./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib -- history
./scripts/cargo-rmcp-patched.sh test --locked -p peri-config --lib -- betas
./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- server_loop_history
```

## 6. 已知限制（必须随发布说明）

1. **`historyRevision` 不是持久结构版本**：本地适配器把它定义为 own 存储序 tail position；
   inherited-only 页沿用会话 own tail。跨实例失效、第二进程改写与重启后的旧身份复用不在承诺内。
2. **legacy 大 inherited 快照需要派生索引**：无索引且快照 >8 MiB 或 payload >4096 条时返回
   `index_not_ready`，需先通过写路径/离线准备派生索引；服务端不隐式全列扫描，也不伪装
   `not_found`/空页。
3. **opaque 正文边界**：单条 canonical payload 读取上限 8 MiB，超限 `budget_exceeded`；详情每次
   请求重新读取并投影正文，存在内存峰值；工具输入单条超过 64 KiB 时按展示规则截断。
4. **附件/图片类 payload 可能投影为空**：非文本 content chunk 在 V2 页面投影中被跳过；仅含
   图片/附件且无可显示文本的条目可能不出现，按 `bodyRef` 读取时返回 `not_found`。
5. **page descriptor 的 `totalBytes` 只是 canonical 预估**：body 响应的 `totalBytes` 才是投影展示
   长度的权威；客户端必须在首块到达后修正滚动范围。
6. **TUI 状态是进程级**：`HISTORY_V2_STATE` 由单进程内多个连接共享；detached 后需要手动重新
   initial/open，没有自动 gap 恢复。
7. **连接循环仍有长操作**：`_peri/history` 读已移到连接所有权任务并受 4 个在途配额约束；
   `session/load` 的 initial 仍在接收循环内 `await`（最长 60 s），本轮未改。
8. **`_peri/history/changed` 未发送**：窗口失效依赖后续请求错误与重新 `open`；没有服务端主动
   失效通知。
9. **远端/多实例范围外**：remote/emscripten 的 `supports_history_v2()` 为 false，协商显式失败；
   本轮不声称这些后端可用。
10. **TUI 出站调度器逐条等待**：调度任务按入队顺序 `send + await`，慢 page/body 会延迟后续
    close/load（慢请求最长 10 s）；FIX-04 只解决服务端接收循环阻塞，不改变客户端串行等待。
11. **页面条目的 `contentVersion` 不用于冲突检测**：页面条目该字段恒为 `"1"`；body chunk 的
    `contentVersion` 是 canonical hash，用于分块自校验。
12. **body 分块重复投影**：每个分块都会重读并重投影最多 8 MiB canonical；128 个 64 KiB 块
    约产生 1 GiB 读取量，单次请求有界，仍需后续优化。

## 7. 回滚

1. 回退到上一软件版本或撤销本次提交；不存在“关闭 beta 回到旧协议”的路径。
2. 服务端侧无需迁移：V2 不新增表/列，窗口与实时流均为进程内状态；服务端重启即清空。
3. 标准 ACP 客户端的完整回放语义未变；回滚不涉及数据格式转换。
4. 残留 V2 会话状态随 TUI 重启消失，不需要清理持久化数据。

## 8. 发布门禁

| 门禁 | 要求 | 本轮记录 |
| --- | --- | --- |
| 旧协议退出 | 旧能力/方法/通知不可达 | 静态命中已核对；负向测试存在 |
| 本地链路 | initial/page/seek/body/close 有端到端证据 | `history_v2_end_to_end_test` 两例 |
| 修复回归 | FIX-01～05 各有可定位用例 | 见 §3 |
| 文档一致性 | 设计、索引、验收与 CLAUDE 路由同步 | 本轮更新 |
| 远端/多实例 | 显式失败且不标为通过 | 范围外 |

History V2 是本地 TUI 的唯一历史路径；发布判断仍须读取 §2 的未覆盖与部分覆盖项，不能把
“旧协议已删除”表述为“完整设计已验收”。
