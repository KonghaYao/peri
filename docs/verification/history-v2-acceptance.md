# History V2 验收与发布说明

> 状态：本地 beta（`history-v2`，默认关闭）验收记录。
>
> 依据：仓库外设计稿《Peri ACP 统一历史加载协议详细设计》v2.0（2026-10-22）§2、§9、§12、§13、§15；
> 本文只记录可复现证据，**不把未验证项写成通过**。验收编号沿用设计稿 A01–A44。

## 1. 范围

1. 仅覆盖**本地 SQLite 后端 + TUI 本地 beta `history-v2`**（默认关闭，显式 `true` 才在
   `initialize` 请求 `_meta["peri.history"]`）。
2. V1 默认路径不变：未开启 beta 或服务端未回显 V2 时，`session/load` 回放、`historyWindowV1`
   与旧分页语义保持原样；V1 wire/行为未改。
3. 远端（remote）与多实例**不宣告** V2：`supports_history_v2` 默认 `false`，未覆写的后端在
   协商阶段返回 `unsupported_version`，不会静默降级成标准全量回放。
4. 不包含：数据库表结构变更、V1 删除、跨实例接管、Agent 执行恢复、持久化执行租约。

## 2. 验收状态（A01–A44）

状态口径：

- **已覆盖**：核心通过标准有直接回归测试。
- **部分覆盖**：只有分层/局部证据，设计要求的规模、并发或端到端场景缺失。
- **未覆盖**：无实现或无回归证据。

| ID | 状态 | 证据 / 缺口 |
| --- | --- | --- |
| A01 | 部分覆盖 | 页预算与 descriptor 有界：`history_v2_adapter_test::open_latest_reports_tail_cursors_and_ordered_entries`、`history_service/service_test::oversized_page_is_budget_exceeded_and_keeps_window`。10 万条会话与 active 恢复成本未实测（`peri-resources` 的 `history_perf_tests` 全部 `#[ignore]`，且针对 V1 窗口读取）。 |
| A02 | 部分覆盖 | `history_v2_adapter_body_test::oversized_payload_above_limit_returns_budget_exceeded_with_bounded_reads` 证明 >8 MiB 单条正文不整读、body 明确 `budget_exceeded`；100 MiB 真实工具输出仅以 opaque descriptor 出现在首屏的场景未构造。 |
| A03 | 部分覆盖 | 一条 canonical payload 的子项全量投影进同一 `entryId`（`history_v2_adapter/view.rs::loaded_entry`），响应总字节由 `budget_exceeded` 兜底；设计要求的「正规化为多个稳定展示 entry、每个语义子项独立可达」未实现，子项级分页未覆盖。 |
| A04 | 部分覆盖 | 不可见条目跳过与 progress：`history_v2_adapter_test::system_and_model_only_entries_are_not_projected`；空页 cursor 严格前进未用 5000 条不可见消息单测。 |
| A05 | 已覆盖 | `history_service/service_test::{same_key_same_fingerprint_replays_success,same_key_different_fingerprint_is_request_conflict}`、`initial_test::initial_replays_same_bytes_and_rejects_conflicting_params`、`history_v2_load_test::session_load_v2_same_request_key_replays_initial_bytes`。 |
| A06 | 部分覆盖 | `history_service/body_test::close_session_closes_only_that_sessions_windows`；窗口/服务按连接隔离（`ConnectionContext` 每连接一个 `HistoryV2Service`/`LiveDelivery`）。同一会话两条真实连接、两个窗口的 e2e 未建立。 |
| A07 | 部分覆盖 | 首屏字节重放见 A05；`handle_load_shared` 仍先执行 `prepare_shared`，未断言「重试不重复 runtime/MCP 装配」。 |
| A08 | 已覆盖 | `initial_test::{initial_backend_failure_reports_effect_and_leaks_no_window,initial_failure_without_execution_ready_hides_runtime_generation}`、`history_v2_load_test::session_load_v2_initial_failure_returns_effect_without_execution`。 |
| A09 | 已覆盖 | 服务端：`live_delivery_test::paused_updates_are_buffered_until_resume`；TUI：`live_test::{admission_accepts_only_contiguous_transcript_sequence,pending_buffer_dedupes_sorts_and_drops_oldest_on_overflow}`。首屏与 live 的并发到达由分层测试覆盖，e2e 未制造竞态。 |
| A10 | 已覆盖 | `live_delivery_test::{count_overflow_drops_oldest_and_exposes_gap,byte_overflow_keeps_buffer_bounded}`、`history_v2_view_test::detached_keeps_the_accepted_prefix_and_shows_a_gap_warning`；溢出显式暴露缺口，不伪装连续。 |
| A11 | 部分覆盖 | `_peri/history` 在 `server_loop.rs::handle_history_request` 独立分流，不取生命周期锁；同类证据 `history_page_rejection_test::a_page_request_is_answered_while_the_session_holds_the_execution_lock`（V1 分页）。V2 在真实执行中并发 page/body 未单测。 |
| A12 | 部分覆盖 | load 生命周期忙返回 `-32010` 的路径存在（`history_page_rejection_test` 的关闭/执行准入），V2 initial 在执行中返回 `busy` 未单测。 |
| A13 | 已覆盖 | TUI：`paging_test::{late_response_from_old_request_does_not_touch_new_state,response_from_an_old_window_is_discarded_and_resets_in_flight}`、`consumer_test::{late_failure_from_previous_session_does_not_touch_new_window,late_open_failure_keeps_the_new_session_claim}`。 |
| A14 | 已覆盖 | `paging_test::seek_preempts_page_and_old_page_cannot_clear_the_new_request`、`body_test::late_body_response_does_not_write_into_a_newer_request`、`history_v2_view_test::same_epoch_page_keeps_detail_but_seek_closes_it`。 |
| A15 | 部分覆盖 | 锚点失效与 gap：`paging_test::{anchor_becomes_unavailable_after_its_entry_is_evicted,coverage_gap_is_explicit_and_never_treated_as_continuous}`；seek 后保留 live 尾部：`history_v2_view_test::{seek_keeps_live_tail_and_marks_it_as_live,seek_dedupes_live_message_already_in_the_new_page,reopen_keeps_streaming_content_not_in_the_new_page}`。终端 resize 下的视觉锚点未自动化。 |
| A16 | 部分覆盖 | `history_v2_adapter_test::tool_call_and_result_share_invocation_key_across_entries`、`history_v2_view_test::{tool_call_and_result_merge_across_entries,tool_result_without_call_is_a_standalone_card,different_invocation_keys_do_not_merge}`。调用/结果被分页切开、跨 source 同名 ID 未单测。 |
| A17 | 已覆盖 | `history_v2_adapter_test::append_stays_outside_the_fixed_tail_and_tail_removal_invalidates`、`service_test::{backend_revision_change_invalidates_and_removes_window,backend_committed_tail_change_invalidates_and_removes_window}`。 |
| A18 | 部分覆盖 | 返回前复核与 close 竞态：`service_test::{backend_revision_change_invalidates_and_removes_window,close_during_page_discards_stale_page}`；V2 下 rewind/delete 的 host 端到端未单测（V1 有 `history_page_rewind_test` / `requests_rewind_inherited_region_test`）。 |
| A19 | 未覆盖 | 适配器明确 `historyRevision` 是 own 存储序 tail position，不是持久结构版本，跨实例失效不在承诺内；无第二进程改写/服务端重启用例。 |
| A20 | 部分覆盖 | fixed tail 与 tail 删除失效见 A17；V2 下 compact（含失败）的提交观察、原文回查与 live 不重置未建 e2e，仅有 V1 `history_window/commit_tail_test`。 |
| A21 | 部分覆盖 | inherited 顺序、跨区翻页、冻结前缀、oversize 前缀判定与无索引失败：`history_v2_adapter_inherited_test::{open_latest_orders_inherited_before_own_and_keeps_own_tail,before_pages_cross_regions_without_duplicates,inherited_oversized_visibility_uses_bounded_prefix,own_append_does_not_change_the_frozen_prefix_and_tail_delete_invalidates,oversized_inherited_without_index_maps_to_index_not_ready}`；父会话删除场景未单测。 |
| A22 | 部分覆盖 | `history_v2_adapter_inherited_test::oversized_inherited_without_index_maps_to_index_not_ready`、`initial_test::initial_without_execution_ready_still_returns_page`、TUI `initial_test::execution_not_ready_keeps_history_readable_but_not_executable`；只读 SQLite 与执行目录不存在未构造。 |
| A23 | 部分覆盖 | `service_test::{page_wrong_ticket_is_ticket_invalid,page_wrong_snapshot_token_is_snapshot_invalidated,page_session_mismatch_is_permission_denied}`、`history_service/cursor_test`（MAC/窗口/旧 cursor）、`body_test::body_ref_must_be_issued_by_the_window`。窗口有效期内权限撤销未单测。 |
| A24 | 已覆盖 | `service_test::{close_then_page_is_window_closed_and_close_replays,close_during_page_discards_stale_page}`、`body_test::body_returns_chunk_and_close_rejects_later_body`、`history_v2_end_to_end_test` 的 close 阶段。 |
| A25 | 已覆盖 | `service_test::{idle_window_expires_after_five_minutes,absolute_window_expiry_wins_over_recent_use,opening_placeholder_is_pruned_after_sixty_seconds,capacity_rejects_ninth_window_and_expiry_frees_a_slot}`。 |
| A26 | 部分覆盖 | TUI 保留预算与失效：`paging_test::{retention_bounds_retained_items_and_keeps_reload_cursor,anchor_becomes_unavailable_after_its_entry_is_evicted}`、`body_test::body_budget_rejects_before_allocating_unbounded_state`；长时间滚动 RSS 与 pin 阻塞可见性未实测。 |
| A27 | 部分覆盖 | TUI 分块拼接与偏移：`body_test::{body_chunks_concatenate_in_offset_order_and_stop_when_complete,late_body_response_does_not_write_into_a_newer_request}`；V2 wire 的多字节 UTF-8 跨块、超大 offset 与 base64 非法输入未单测（V1 详情有 UTF-8 重组测试）。 |
| A28 | 已覆盖 | `history_v2_load_test::{session_load_v2_legacy_response_is_unchanged_and_missing_meta_is_rejected,session_load_v2_mixed_fields_and_unnegotiated_initial_are_rejected}`、`history_page_caps_test::history_v2_initialize_negotiates_on_local_sqlite_and_legacy_still_works`；协商纯规则另见 `requests/session_caps.rs` 内联测试。 |
| A29 | 已覆盖 | 旧客户端：`history_page_caps_test::history_v2_initialize_negotiates_on_local_sqlite_and_legacy_still_works` 证明仅声明旧能力仍成功、不回显 `peri.history`；`history_v2_load_test::session_load_v2_legacy_response_is_unchanged_and_missing_meta_is_rejected` 证明 V1 回放不变。新客户端连旧后端：`requests_caps_test::test_history_v2_declaration_requires_explicit_server_selection` 证明 fail-closed，不回落全量回放。 |
| A30 | 部分覆盖 | 本地真实 SQLite 全链路：`history_v2_end_to_end_test`、`history_v2_adapter_*_test`；remote 未实现 V2（fail-closed），未记录跨实现读取量/往返。 |
| A31 | 已覆盖 | 新客户端（beta 开启）握手只得到旧私有能力时，`test_history_v2_declaration_requires_explicit_server_selection` 明确报错并阻止启动；服务端侧 `negotiate_history_v2` 不会把旧能力声明升级成 V2，也不会在缺少 `peri.history` 时静默声明 V2。 |
| A32 | 部分覆盖 | `replay_initial` 复核 `execution_ready`、`runtimeGeneration` 与流身份；`initial_test::initial_replays_same_bytes_and_rejects_conflicting_params` 覆盖同态重放；runtime 已关闭后的重试未单测。 |
| A33 | 部分覆盖 | `initial_test::{initial_delivery_expires_after_ledger_eviction,initial_delivery_expires_with_its_window}`；客户端暂存已丢失但服务端仍可重放的场景未单测。 |
| A34 | 已覆盖 | TUI：`consumer_test::{window_expiry_requires_reopen_and_reopen_enqueues_open,snapshot_invalidated_resets_claim_and_reopen_replaces_the_window,seek_advances_epoch_and_replaces_retained_window}`、`history_v2_view_test::{window_expiry_renders_warning_note_and_keeps_retained_pages,full_flow_pages_seeks_reads_body_and_survives_window_expiry}`。 |
| A35 | 未覆盖 | `_peri/history/changed` 只出现在协商回显，服务端未发送 committed/invalidated/closed；无 itemSeq 封口证明与多连接恢复协议。 |
| A36 | 未覆盖 | 没有 V2 派生目录的构建、续建、版本与 GC；当前适配器直接复用窗口读取端口。 |
| A37 | 部分覆盖 | `service_test::oversized_page_is_budget_exceeded_and_keeps_window`、类型层 `history_v2_test::progress_budget_exhaustion_requires_progress`；超小预算、元数据超大、成功空页无进展未专门压测。 |
| A38 | 已覆盖 | `service_test::{ledger_byte_budget_evicts_success_and_retry_expires,gap_then_old_key_is_operation_expired}`、`initial_test::initial_delivery_expires_after_ledger_eviction`。 |
| A39 | 部分覆盖 | `history_service/cancel_test::oversized_params_are_invalid_before_decode_and_state_registration`（64 KiB 入站上限）与类型层未知字段/范围校验；深嵌套、4096 元素、并发洪峰未测。 |
| A40 | 部分覆盖 | `history_v2_adapter_test::oversized_entry_is_opaque_and_body_returns_projected_text`（分块、`complete`、`nextOffset`）、`service_test::{body_invalid_offset_and_zero_max_are_invalid_params,body_over_limit_is_invalid_params}`；空未完成块、提前 EOF、精确 EOF 空块未单测。 |
| A41 | 已覆盖 | `service_test::{concurrent_same_key_returns_operation_in_progress,in_progress_can_be_retried_after_120_seconds}`、`cancel_test::{cancelled_open_releases_placeholder_and_allows_immediate_retry,request_timeout_releases_placeholder_and_allows_immediate_retry}`。 |
| A42 | 部分覆盖 | 只发布 sealed 行并带 `finalItemSeq`（`history_v2_adapter_test::open_latest_reports_tail_cursors_and_ordered_entries`）；客户端封口证明、迟到 delta、partial 行封口协议未实现。 |
| A43 | 部分覆盖 | fixed tail 与 append 后新 open 可见（A17 同源测试）；显式 durable pin / GC 协调未实现。 |
| A44 | 已覆盖 | `initial_test::initial_replaces_same_session_windows_and_leaves_others_live`、`history_v2_load_test::session_load_v2_repeated_loads_do_not_exhaust_window_capacity`、`history_v2_end_to_end_test::history_v2_end_to_end_repeated_loads_replace_window_without_capacity_error`、`live_delivery_test::{close_removes_stream_and_later_updates_pass_through,close_if_paused_never_tears_down_an_active_stream}`、TUI `consumer_test::open_latest_enqueues_close_before_open_when_a_window_exists`。 |

**汇总：已覆盖 16 项，部分覆盖 25 项，未覆盖 3 项（A19、A35、A36）。**

发布判断必须读取上表：未覆盖与部分覆盖项**不能**被表述为 V2 已满足完整设计验收。高风险缺口：

1. A03：子项级正规化目录未实现，单条 canonical 含海量子项时只能整条返回或 `budget_exceeded`。
2. A19 / A35 / A36：跨实例失效、`_peri/history/changed` 与封口证明、V2 派生目录构建/GC 均未实现。
3. A07 / A18 / A20 / A30：load 幂等装配、rewind/compact 端到端、remote 合同尚无完整回归。
4. A11 / A12：V2 与真实执行并发的 page/body、执行中的 initial busy 未单测。

## 3. 本轮验证（2026-10-22，worktree `history-v2`）

全部命令在 `/Users/konghayao/code/ai/peri/.worktrees/history-v2` 执行；结果为**独立复跑**，
不是子代理报告的直接引用。

| 命令 | 结果 |
| --- | --- |
| `./scripts/cargo-rmcp-patched.sh fmt -p peri-acp -p peri-acp-types -p peri-resources -p peri-tui` | 通过（无输出） |
| `./scripts/cargo-rmcp-patched.sh check --locked -p peri-acp -p peri-acp-types -p peri-resources -p peri-tui` | 通过（0.43s，热态） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp-types --lib` | 714 passed，0 failed，0 ignored |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-resources --lib` | 593 passed，0 failed，34 ignored |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib` | 1204 passed，0 failed，1 ignored |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- history_v2_end_to_end` | 2 passed，0 failed |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui --lib` | 首次失败后连续 10 次通过：每次 2389 passed，0 failed，9 ignored |
| `./scripts/check-changed.sh` | 通过；脚本判定 `cargo check --locked --workspace`（7.70s） |
| `bash scripts/verify-async.sh read` | `verdict: pass`（`check --all-targets` 6s，`clippy --all-targets -D warnings` 19s；回执快照 fresh） |

TUI 稳定性说明：本轮第一次全量 TUI 运行出现 1 个失败
（`acp_client::client::requests::session_request_tests::failed_new_session_does_not_enqueue_window_open`，
2388 passed / 1 failed）。该用例单独复跑 5/5 通过，随后全量连续 10 次通过（含同类全局 atom
夹具的其他用例）。该失败与 History V2 路径无关，但属于未完全复现的测试隔离抖动，发布记录须保留。

另有 1 次 `peri-acp` 全量运行出现 1 个失败（1203/1204），与后台全量 check/clippy 并行执行；
该次输出未保留失败测试名，随后连续 3 次 1204 passed / 0 failed。该失败不可复现、不能归因，
不作为 V2 行为证据，也不掩盖为“全绿”。

## 4. 已知限制（必须随发布说明）

1. **`historyRevision` 不是持久结构版本**：本地适配器把它定义为 own 存储序 tail position；
   inherited-only 页沿用会话 own tail。跨实例失效、第二进程改写与重启后的旧身份复用不在承诺内。
2. **legacy 大 inherited 快照需要派生索引**：无索引且快照 >8 MiB 或 payload >4096 条时返回
   `index_not_ready`，需先通过写路径/离线准备派生索引（本轮未提供独立索引 CLI）；服务端不隐式
   全列扫描，也不伪装 `not_found`/空页。
3. **opaque 正文边界**：单条 canonical payload 读取上限 8 MiB，超限 `budget_exceeded`；详情每次
   请求重新读取并投影正文，存在内存峰值；工具输入单条超过 64 KiB 时按展示规则截断。
4. **附件/图片类 payload 可能投影为空**：非文本 content chunk 在 V2 页面投影中被跳过；仅含
   图片/附件且无可显示文本的条目可能不出现，按 `bodyRef` 读取时返回 `not_found`。
5. **page descriptor 的 `totalBytes` 只是 canonical 预估**：body 响应的 `totalBytes` 才是投影展示
   长度的权威；客户端必须在首块到达后修正滚动范围。
6. **TUI 状态是进程级**：`HISTORY_V2` / 窗口状态在单进程内由多个连接共享；detached 后需要手动
   重新加载（重新 initial/open），没有自动 gap 恢复。
7. **`_peri/history` 串行处理**：连接循环按请求逐条处理，没有 wire 级并发；并发窗口的吞吐受
   单连接调度限制。

## 5. 回滚

1. 关闭 beta：将 `history-v2` 设为 `false` 或不配置（默认关闭），TUI 不再请求
   `_meta["peri.history"]`，回到 V1 `historyWindowV1` / 旧分页 / 标准回放路径。
2. 服务端侧无需迁移：V2 不新增表/列，窗口与实时流均为进程内状态；服务端重启即清空。
3. V1 wire/行为未变；回滚不涉及数据格式转换。
4. 残留 V2 会话状态随 TUI 重启消失，不需要清理持久化数据。

## 6. 发布门禁

| 门禁 | 要求 | 结果 |
| --- | --- | --- |
| 格式 | `fmt` 四 crate | 通过 |
| 类型 | `check --locked` 四 crate + `check-changed.sh` | 通过（后者实际执行 workspace check） |
| 全量编译与 lint | `verify-async.sh`：`check --all-targets` + `clippy --all-targets -- -D warnings` | 通过（回执 `verdict: pass`；修复了 R1/R2 遗留的 4 个 clippy 问题） |
| 单元/契约 | `peri-acp-types`、`peri-resources`、`peri-acp` 全量 lib | 通过（714 / 593 / 1204） |
| TUI 稳定性 | 全量 lib 连续 3 次 | 通过（首次运行后连续 10 次；首次运行有 1 次无关抖动，见 §3） |
| wire 端到端 | `history_v2_end_to_end` | 通过（2/2） |
| 变更门 | `scripts/check-changed.sh` | 通过 |

V2 仍为**默认关闭的 beta**；GA 门禁额外要求关闭 A03、A19、A35、A36 与 A07/A18/A20/A30
的缺口，并补齐 A11/A12 的并发回归。
