# Langfuse telemetry spool：P5 实测报告

- 状态：P1–P5 已实施。P4b 宿主装配已落地：本轮（2026-10-12 19:20 起）实测 `peri-acp` 编译与测试通过，workspace `--all-targets` 全量闸门 exit 0，各 crate 测试全绿（§2.1）。真实 Langfuse 端点演练、512 MiB 积压 RSS 与性能测量仍未执行，不宣称产品验收完成（§3、§4）。
- 基线 commit：`607f8a222a4abaf37a7d3d68863cd244a1a37bb3`（worktree `.worktrees/peri-telemetry-spool`，未提交任何改动）。
- 契约与计划：同目录 `2026-10-12-langfuse-telemetry-spool-contract.md`、`2026-10-12-langfuse-telemetry-spool.md`。
- 命令时间：§2 是 2026-10-12 18:00–18:16 的历史快照（当时 P4b 未落地，红点见该表，已由后续交付关闭）；§2.1 是本文档与验收表同步流在同一日 19:20 之后实际重跑的结果，两者都以真实输出为准。
- 本文只记录实际执行过的命令与结果；未执行的项一律标注为“未测/缺口”，不代替真实端点验收。

## 1. 运行环境

| 项 | 值 |
| --- | --- |
| OS | macOS 26.5.1（Build 25F80），Apple M5 Pro，48 GiB 内存 |
| Rust | rustc 1.99.0 (b940084d8)，cargo 1.99.0 (5f94df4) |
| 平台 triple | 原生 `aarch64-apple-darwin`；WASM 目标 `wasm32-unknown-emscripten`（Emscripten 6.0.10，`~/.emsdk`） |
| 构建/测试包装 | `./scripts/cargo-sccache.sh`（`cargo-sccache.sh fmt/check/test`）、`scripts/cargo-wasm.sh`、`scripts/cargo-rmcp-patched.sh` |

## 2. 实际执行的命令与结果

| 命令 | 结果（输出末尾） |
| --- | --- |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client` | `test result: ok. 276 passed; 0 failed; 1 ignored`（lib）；`tests/logging.rs` 1 passed；`tests/otel_contract.rs` 1 passed；doc-tests 0 passed；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-config --lib` | `test result: ok. 159 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.04s`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-controller --lib langfuse` | `test result: ok. 158 passed; 0 failed; 0 ignored; 0 measured; 16 filtered out`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-controller --test langfuse_e2e` | `test result: ok. 5 passed; 0 failed; 0 ignored`；EXIT=0 |
| `./scripts/cargo-sccache.sh check --locked -p peri-acp --all-targets`（18:12 首次） | **EXIT=101**：3 处 `error[E0061]`，均为 `LangfuseSession::new_owned` 缺第 3 参 `Option<Arc<dyn SpoolStore>>`（`peri-acp/src/host/assemble.rs:913` lib 与 lib test；`peri-acp/src/host/stdio/langfuse_shutdown_test.rs:27`） |
| `./scripts/cargo-sccache.sh check --locked -p peri-acp --all-targets`（18:16 复跑） | **EXIT=101**：E0061 已消失（`assemble.rs` 已迁移调用），改为 `error[E0308]`（`peri-acp/src/host/telemetry.rs:45`，`Arc::clone(&session)` 期望 `&Arc<dyn LangfuseSessionLike>`）与 `couldn't find file peri-acp/src/host/telemetry_test.rs`（`telemetry.rs:112` 挂载 `mod tests;`）；P4b 仍在写，`peri-acp` 仍未编译 |
| `./scripts/cargo-sccache.sh fmt --all -- --check` | EXIT=0（无 diff 输出） |
| `bash scripts/check-layer-imports.sh` | `§0 依赖门：共 22 条规则，违规边 0`，全边通过；EXIT=0 |
| `bash scripts/check-file-size.sh` | EXIT=1：3 个超限文件均为**既有**（`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-tui/src/kit/acp_events/system.rs` 1007、`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038），本次未新增 |
| `git diff --check` | EXIT=0 |
| `scripts/cargo-wasm.sh check --locked --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare` | **EXIT=101**：闭包内 `langfuse-client` 编译通过（13 warnings），随后 `peri-acp` 因同一 `new_owned` 缺参中止；未到达 `peri-wasm`。详见 §4 |

上表“未执行”指 18:00–18:16 快照时点；其中 workspace 全量闸门已在 §2.1 补跑。真实 Langfuse 端点演练与人工测量仍未执行（§4）；`cargo clippy -D warnings` 与 E2E（`e2e/`）本轮仍未执行。

### 2.1 本轮重跑（文档与验收表同步流，2026-10-12 19:20 之后）

| 命令 | 结果（输出末尾） |
| --- | --- |
| `./scripts/cargo-sccache.sh check --locked --workspace --all-targets` | `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 22.95s`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client --lib` | `test result: ok. 282 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 17.31s`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client --test logging --test otel_contract` | `tests/logging.rs`：`1 passed`；`tests/otel_contract.rs`：`1 passed`；0 failed |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client --test spool_http`（该文件 19:45 由并行流新建，本流 19:5x 实测） | `running 3 tests` → `test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.69s`；EXIT=0（含 `retry_reopens_at_zero_after_mid_body_disconnect`） |
| `./scripts/cargo-sccache.sh test --locked -p peri-config --lib` | `test result: ok. 159 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.94s` |
| `./scripts/cargo-sccache.sh test --locked -p peri-controller --lib` | `test result: ok. 180 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.73s` |
| `./scripts/cargo-sccache.sh test --locked -p peri-controller --lib langfuse` | `test result: ok. 164 passed; 0 failed; 0 ignored; 0 measured; 16 filtered out; finished in 5.22s` |
| `./scripts/cargo-sccache.sh test --locked -p peri-controller --test langfuse_e2e` | `test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s` |
| `./scripts/cargo-sccache.sh test --locked -p peri-acp --lib` | `test result: ok. 827 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 90.20s` |
| `./scripts/cargo-wasm.sh check --locked --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare` | `Checking peri-wasm v0.2.0` → `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 43.13s`；EXIT=0（复跑缓存命中 0.75s、EXIT=0）。emscripten 闭包告警：`langfuse-client` 5、`peri-resources` 14、`peri-middlewares` 17、`peri-acp` 1，无 error |
| `bash scripts/check-layer-imports.sh` | `§0 依赖门：共 22 条规则，违规边 0`；EXIT=0 |
| `bash scripts/check-file-size.sh` | **EXIT=1**（脚本按“存在超 1000 行文件”返回非零）：`共扫描 2274 个文件，超阈值 3 个（源码 2 / 测试 1）`——`peri-tui/src/kit/panels/mcp.rs` 1637、`peri-tui/src/kit/acp_events/system.rs` 1007、`peri-resources/src/sessions/sqlite_store/schema_test.rs` 1038，均为既有文件、不在本变更集 |
| `./scripts/cargo-sccache.sh fmt --all -- --check` | **EXIT=1**：3 处未格式化——`langfuse-client/src/batcher/admission_test.rs:225`、`langfuse-client/src/batcher/pending_test.rs:540`、`peri-tui/src/acp_client/deployment_test.rs:122`；属源码流（跨流改动请求见 §8.3），本流只改文档、不代改 |
| `git diff --check` | EXIT=0（无空白错误） |

测试计数与 §2 早前快照不同（276→282、174→180）：P4b 与集成流在 18:16 之后继续新增/调整用例，以本轮实测为准。

复跑复核（同日 19:5x 再快照）：`check --locked --workspace --all-targets` EXIT=0（缓存命中 0.92s）、`-p langfuse-client --lib` 282 passed（16.79s）、`-p peri-config --lib` 159 passed（0.55s）、`-p peri-controller --lib` 180 passed（4.70s）、`-p peri-acp --lib` 827 passed（85.26s）、`fmt --check` 仍 EXIT=1（同一 3 处），§8.3 的行数复测不变。

并发改动说明：本轮执行期间工作树内另有并行流在改 14 个 `.rs` 文件（`find . -name '*.rs' -newermt '2026-10-12 19:15'` 命中 `langfuse-client/src/{client,batcher,client_send,client_send_test}.rs`、`src/batcher/{admission,admission_test,worker,recovery,failure,shutdown,pending_test,batcher_shutdown_test}.rs`、`peri-controller/src/langfuse/tracer/{input_snapshot,input_snapshot_test}.rs`，mtime 19:23–19:35）并新建 `langfuse-client/tests/spool_http.rs`（19:45，本流单独补跑，见上表）；本流未写任何 `.rs`。因此 §2.1 的结果对应该次运行时的磁盘内容，不是单一冻结 revision；两次快照数字一致，仍建议改动停止后按同一命令再复核一次。

## 3. §9 验收表现状

图例：**已自动化**＝有离线测试文件与用例名；**部分覆盖**＝仅单进程/注入式覆盖，缺口已列；**未测**＝无离线用例或需真实条件。本表为 2026-10-12 19:5x 快照（并行流仍在交付：`langfuse-client/tests/` 现有 `logging.rs`、`otel_contract.rs`、`spool_http.rs`）。

| 计划 §9 场景 | 现状 | 证据 / 缺口 |
| --- | --- | --- |
| 默认及关闭（true/false/off、非法值、内存有界） | 已自动化 | `peri-config/src/observability_spool_test.rs::spool_defaults_and_env_precedence`、`new_boolean_and_compression_forms_are_normalized`、`new_integer_keys_reject_zero_negative_float_overflow_and_blank`、`invalid_winning_value_rejects_snapshot`、`disabled_spool_preserves_old_files`；`langfuse-client/src/spool_test.rs::disabled_spool_never_opens_native_and_leaves_the_spool_directory_untouched`；`src/batcher/freeze_test.rs::disabled_spool_never_touches_disk_and_rejects_over_allowance`。缺口：测试只扫临时目录，未扫用户 HOME（有意为之） |
| 内容保真（≥100 边界样本、0 mismatch、非 input 字段不变） | 已自动化 | `src/encoding/stream_test.rs::hundred_boundary_inputs_roundtrip_exact_bytes`、`escaped_utf8_across_chunk_boundary`、`spooled_and_inline_sources_share_one_escaping`、`frozen_baseline_bytes_are_stable`；`src/batcher/send_test.rs::frozen_input_and_output_reach_the_wire_verbatim`。缺口：服务端可查询性需真实端点 |
| 长会话（1000 次调用、约 8 MiB、12/16 MiB 限额） | 未测 | 无离线用例（计划 `langfuse-client/tests/spool_acceptance.rs::thousand_calls_preserve_all_hashes` 不存在）；**需真实端点**确认服务上限与 1000 条可查询 |
| 编码膨胀（原文未超限、OTLP 超限 → 合法拆批） | 已自动化 | `src/batcher/split_test.rs::encoded_expansion_splits_without_losing_events`、`event_count_limit_splits_batches`、`raw_batch_limit_splits_before_the_encoded_limit`；`src/batcher/spool_test.rs::otlp_expansion_splits_without_loss` |
| 超大单条（精确拒收、邻条继续） | 已自动化 | `src/batcher/spool_test.rs::oversized_span_rejects_only_itself`；`src/batcher/split_test.rs::oversized_encoded_span_is_rejected_while_the_rest_continues`、`oversized_raw_span_maps_to_the_frozen_error`、`measured_item_over_the_encoded_limit_is_rejected_not_batched` |
| 慢网络/断网积压（512 MiB、1 GiB quota、受控峰值 ≤32 MiB、RSS ≤64 MiB） | 部分覆盖 | 受控部分：`src/memory_test.rs::retained_saturation_preserves_progress_workspace`、`used_includes_isolated_progress_partition_exactly_once`、`workspace_pools_are_exclusive_and_return_leases`；`src/spool/native_test.rs::quota_includes_writing_and_failed_delete`（8 MiB）；`src/batcher/spool_test.rs::queued_bytes_charges_source_descriptor_not_contents`。缺口：**需人工测量**同机额外 RSS 与 512 MiB 积压，须 allocator/框架账单校准；无离线验收用例文件（`half_gib_backlog_has_constant_memory_window` 不存在） |
| 断网恢复（中断 10 分钟、未过期条目最终送达、期间 flush 不成功） | 部分覆盖 | `src/batcher/recovery_test.rs::retry_wait_reports_persisted_pending`、`pending_counts_survive_repeated_flush`、`ttl_never_resets_on_retry`、`recovery_deferral_keeps_objects_for_the_next_owner`。缺口：**需真实端点演练**保留 10 分钟墙钟断网记录（`ten_minute_outage_recovers_before_ttl` 不存在，虚拟时间不能替代） |
| 文件流重试（中途断连，第二次从字节 0） | 已自动化（离线，19:5x 补齐） | 单元层：`src/client_send_test.rs::file_attempts_rebuild_the_request_from_byte_zero`、`file_attempt_must_be_drained_before_the_next_arm`、`settle_reports_an_unconfirmed_pump_at_the_deadline`。真实 TCP 层（本轮新出现并实测通过，属并行流交付）：`langfuse-client/tests/spool_http.rs::retry_reopens_at_zero_after_mid_body_disconnect`、`file_body_is_served_from_spool_without_transfer_encoding`、`gzip_and_none_requests_match_encoding_and_length`——`./scripts/cargo-sccache.sh test --locked -p langfuse-client --test spool_http` → `3 passed; 0 failed`，EXIT=0。缺口：服务端侧断流的真实网络抖动仍未演练 |
| 崩溃恢复（writing/ready/sending/成功后清理前注入崩溃） | 部分覆盖 | `src/spool/native_test.rs::abort_during_writing_leaves_no_visible_object`、`restart_resumes_enumerable_objects_and_sweeps_residue`、`claim_is_exclusive_across_processes`；`src/batcher/recovery_test.rs::corrupt_entry_is_quarantined_without_send_and_next_round_recovers`。缺口：**需实机/子进程 kill 注入**四个持久边界（`tests/spool_recovery.rs::child_crash_at_each_durable_boundary` 不存在），FS/OS 支持矩阵未测 |
| 配额及清理（8 MiB、TTL 60s、磁盘满与删除失败） | 部分覆盖 | `src/spool/native_test.rs::quota_includes_writing_and_failed_delete`（8 MiB 配额含 writing 与失败删除）、`reserve_is_exclusive_across_processes`；`src/spool/cleanup_test.rs::failed_delete_keeps_claim_and_slot_until_retry_succeeds`、`other_owner_sees_no_claim_while_cleanup_slot_holds_it`、`handoff_disarms_once_without_deleting`；`src/batcher/recovery_test.rs::expired_entry_is_cleaned_without_send`。缺口：`native_test.rs` 模块注释明确临时目录**无法稳定复现 ENOSPC**，改测可复现路径；真实磁盘满/断电耐久须实机，注入不冒充断电 |
| 生命周期（前缀屏障、唯一 join owner、Pending 计数） | 部分覆盖 | `src/batcher/recovery_test.rs::shutdown_cancel_reentry_keeps_owner`、`permanent_failure_and_pending_are_both_reported`；`src/batcher/pending_test.rs::flush_reports_a_retry_waiting_prefix_as_pending`、`quota_rejection_is_counted_and_its_reservation_reclaimed`；`src/batcher/spool_test.rs::flush_prefix_protects_preparing_events`；`src/batcher/freeze_test.rs::disk_failure_falls_back_to_memory_once`。P4b 宿主侧已补齐：`peri-acp/src/host/telemetry_test.rs::host_shutdown_reenters_after_telemetry_pending`、`host_reports_persisted_pending_without_complete`、`host_keeps_resources_until_io_joined`、`shared_session_has_no_shutdown_capability`、`spool_capability_requires_enabled_credentials_and_routable_endpoint`（5 用例，含在 `-p peri-acp --lib` 827 passed 内）。缺口：三策略×各状态的完整驱逐矩阵用例（`backpressure_state_matrix_reclaims_all_owned_resources`）不存在；第二次 shutdown 读到同一冻结终态的契约冲突见 §8.2 |
| 平台差异（原生卸载、关闭 spool、无持久化宿主） | 部分覆盖 | 原生与关闭 spool：同“默认及关闭”行。编译面已通过：`scripts/cargo-wasm.sh check --locked --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare` EXIT=0（`Checking peri-wasm` 后 `Finished`，见 §2.1/§4）。缺口：**需目标宿主运行**——WASM/浏览器 JS/Blob 缓冲峰值与模式真实性；编译通过不等于宿主行为已验证 |

性能补表（单事件冻结/入队耗时、gzip CPU、磁盘写量、P95/P99、排空时间、堆/RSS/JS 峰值）：**未测**，本文不提供任何性能数字。

## 4. 不能离线完成 / 未达成的项

1. **1000 次调用 8 MiB 长会话**：需真实端点确认服务上限与逐条可查询，本地 mock 不能证明。
2. **512 MiB 积压 + 32 MiB 遥测预算**：受控 permit 计数不能替代 RSS 测量，需同机基线与 allocator 校准。
3. **断网 10 分钟恢复**：需真实墙钟演练与真实端点。
4. **崩溃恢复注入**：需真实子进程 kill 与实机 FS 支持矩阵。
5. **平台差异（WASM/无持久化）**：编译面本轮已通过（下方第二轮登记，EXIT=0）；无持久化宿主的 JS/Blob 峰值与模式真实性仍需目标宿主运行。

WASM 登记（历史：18:12 快照，P4b 未落地时失败）：

```
$ scripts/cargo-wasm.sh check --locked --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare
    Checking langfuse-client v0.2.0 (.../langfuse-client)        # emscripten 闭包内编译通过
    ...
error[E0061]: this function takes 3 arguments but 2 arguments were supplied
   --> peri-acp/src/host/assemble.rs:913:15
error: could not compile `peri-acp` (lib) due to 1 previous error
EXIT=101
```

WASM 登记（本轮重跑，2026-10-12 19:2x）：

```
$ scripts/cargo-wasm.sh check --locked --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare
    Checking peri-wasm v0.2.0 (.../peri-wasm)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 43.13s
EXIT=0
```

结论：emscripten 目标已编译到 `peri-wasm`，EXIT=0；18:12 的 E0061 随 P4b `host/telemetry.rs` 落地消失（该次运行 `Checking peri-wasm v0.2.0` → `Finished`，复跑缓存命中 0.75s 仍 EXIT=0）。闭包内仍有告警（`langfuse-client` 5、`peri-acp` 1、`peri-resources` 14、`peri-middlewares` 17，均为 cfg 分支下的 dead_code/unused，属源码流），EXIT=0 只证明该目标编译通过，不证明 WASM 宿主运行行为。

补充说明：不带 `--target` 的 `scripts/cargo-wasm.sh check -p peri-wasm` 会走原生 target，而 `peri-wasm` 的依赖在 `cfg(target_os = "emscripten")` 下，原生检查等于空壳，不能作为 WASM 证据；以带 `--target` 的命令为准（记录更正过程）。

## 5. 失败与修复记录概览

- 实施期红点（P1–P4a 已修复，证据见 §2 的绿灯命令）：配置解析由 `LangfuseConfig` 直返改为 `Result` 并逐键区分胜出来源；`Batcher::try_new` 增加 spool 参数与 config 一致性校验；`SessionLike` 增补 `try_freeze`/`try_add_prepared`/`memory_budget` 并切换 `flush` 返回 `FlushReport`；`new_owned` 增加 `Option<Arc<dyn SpoolStore>>`。
- **当前红点（未修复，属 P4b 范围，状态在变）**：18:12 首次检查为 `peri-acp` 三处 `new_owned` 缺第 3 参（E0061）；18:16 复跑时调用已迁移、E0061 消失，改为 `host/telemetry.rs:45` 的 E0308 与 `telemetry.rs:112` 挂载的 `telemetry_test.rs` 缺失。两种状态下 `peri-acp`、`peri-tui` 及其上层闭包与 workspace 全量闸门均未通过。（2026-10-12 集成流更正：P4b 交付后上述红点已全部关闭，证据见 §7。）
- 遗留待清（消费者已接线，allow 应移除，属源码范围、本流未动）：`langfuse-client/src/{memory,payload,snapshot}.rs`、`src/spool/{types,cleanup}.rs` 顶部 `#![allow(dead_code)]`；`src/types/mod.rs:7` 未使用 import；`types/otlp.rs` 等 dead_code 警告。命令 `check --locked -p peri-acp --all-targets` 的 `langfuse-client` 输出为 `generated 13 warnings`。（2026-10-12 集成流已清理，见 §7。）
- 红点关闭复核（本轮 §2.1）：workspace `--all-targets` EXIT=0、`peri-acp --lib` 827 passed，E0061/E0308 与缺失测试文件均已消失；`fmt --check` 转为新的未通过项（3 处，见 §2.1 与 §8.3）。
- 文档期更正：本报告初稿曾以原生 `cargo-wasm.sh check` 记 WASM 通过，核对该脚本不注入 `--target`、且 `peri-wasm` 依赖受 `cfg(target_os = "emscripten")` 保护后判定为无效证据，已改为带 `--target wasm32-unknown-emscripten` 的命令。
- 改动范围核对（放行闸门）：本流只写入 5 个 docs 文件（`environment-variables.md`、`architecture-contracts.md`、`code-index/{langfuse-client,peri-controller,peri-config}.md`）与 2 个 spec 文件（报告、契约 §1/§9 回填），未写任何 `.rs`/`Cargo.toml`。`git diff --stat` 中的源码改动均早于本流（见 §2 命令时间与文件 mtime 对比）。

## 6. 交接与后续

- 契约 §9 已按本文回填“实际状态”列；§1 状态更新为“已实施，待用户验收”。
- P4b 交付后本可执行、且本轮已执行：`peri-acp --all-targets` 与全 workspace 闸门（§2.1）、`host/telemetry_test.rs` 的跨两次 shutdown 用例（827 passed 内含）、ACP 侧 Pending 旁路用例（`peri-tui/src/acp_client/deployment_test.rs::test_deployment_telemetry_pending_never_changes_business_result`）。
- 新增未通过项与遗留：`cargo fmt --check`（3 个源码文件）、`cargo clippy -D warnings` 未跑、`e2e/` 未跑；驱逐一类的矩阵用例仍未建（§3 生命周期行）。
- 需要真实环境或人工测量才能关闭的验收：§3 中标“未测/需真实端点/需人工测量”的各行。

## 7. 集成流清遗留与仓库级复核（2026-10-12，全局集成流）

- 五个文件级 `#![allow(dead_code)]` 全部移除；`--all-targets` 下 `langfuse-client` 警告数 22 → 0。只被单元测试消费的符号改为 `#[cfg(test)]` 门控：`WorkspaceLease::{bytes 字段, bytes()}`、`BudgetedBytes::into_shared`、`ExportPayload::body_len`、`SplitPlan::{planned_len, accounted_len}`、`BatchWindow::is_empty`、`CleanupRegistry::disarmed`、物化回归路径（`types` 的 `ingestion_events_to_otel` 重导出、`EventView::bare`、`encoding::write_views`、`otlp.rs` 的 request/resource/scope 包装类型）。
- 无任何消费者（含测试）的符号保留实现并逐项归因，不用文件级 allow：`CleanupRegistry::changed()`（契约 §3.2 的 worker 唤醒点，当前 `drain` 由 worker 周期触发，尚无 awaiter）、`CleanupSlot::seq` 字段（契约 §3.2「关联序号」，只写不读；唯一读取入口 `CleanupSlot::seq()` 无任何调用方，已删除）、`SlotState::Disarmed::handed_off`（写入不读，移交计数另由 `disarmed_after_handoff` 记录）、`CleanupRegistration::disarm_after_handoff()`。四处归因写在 `spool/cleanup.rs` 文件头，不放大到文件级 allow。`otlp.rs` 的 `OtelAttributeValue::{string,int}` 与 `OtelAttribute::{new,string}` 无任何引用，已删除；`spool/types_test.rs::FakeClaim.id`（测试内只写不读）已删除。
  - 更正（本轮复核）：`disarm_after_handoff` 的归因「生产 §5.2 移交分支尚未接线」不成立——`langfuse-client/src/batcher/recovery.rs::release_hand_off`（:314，非 `#[cfg(test)]`）在 shutdown 移交路径调用它，调用链 `Batcher::shutdown` → `WorkerExit::retained` → `RetainedOwner::converge` → `plan_hand_off`/`release_hand_off`。因此 `spool/cleanup.rs` 文件头注释与 `#[allow(dead_code)]` 已过时（跨流请求见 §8.3）。
- 仓库级复核实际结果：`check --locked --workspace --all-targets` exit 0（2m03s，0 warning/0 error）；`test --locked -p langfuse-client` 276+1+1 passed、`-p peri-config` 159 passed、`-p peri-controller` 174+5 passed、`-p peri-acp` 837 passed（6 target），全部 0 failed；`scripts/check-layer-imports.sh` 违规边 0；`scripts/check-file-size.sh` 报 3 个超限文件（`peri-tui/src/kit/panels/mcp.rs`、`peri-tui/src/kit/acp_events/system.rs`、`peri-resources/src/sessions/sqlite_store/schema_test.rs`），均不在本变更集内（`git status` 未触及），属既有状态。

## 8. 文档与验收表同步流（2026-10-12 19:20 起）

### 8.1 本轮改动（只写文档与 spec，未改任何 `.rs`/`Cargo.toml`）

1. `docs/code-index/langfuse-client.md`：修正三条被点名的过时描述——grace 归属与语义（在 `Batcher::shutdown`，到期返回 `Pending{counts}` 且保留 owner）；删除与移交两条 disarm 路径分开（delete 成功 `disarm_after_delete`；shutdown 移交由 `batcher/recovery.rs::release_hand_off` 调 `disarm_after_handoff`）；`ingestion_events_to_otel` 改标为 `#[cfg(test)]` 物化回归入口，生产编码指向 `encoding::write_otlp`。另补 `freeze/prepare/split`、`result/recovery` 行的真实符号，并把「生产接线」更新到 `peri-acp/src/host/telemetry.rs::assemble` 与 `host/telemetry_test.rs`。
2. `docs/code-index/peri-controller.md`、`docs/code-index/peri-config.md`：逐条核对已落地符号（`observability_spool.rs` 11 键、`ENVIRONMENT_KEYS` 24 项、`session_like`/`input_snapshot`/`llm_events` 入口、`session.rs::map_shutdown`/`retains_ownership`、`LangfuseShutdownOwner`）后未发现新偏差，本轮未再改这两份文件。
3. 本报告：§2.1 补真实重跑证据；§3 生命周期与平台差异两行更新；§4 重登记 WASM（由「未通过」改为 EXIT=0）；§5/§6 记录红点关闭与新未通过项；§7 更正 `disarm_after_handoff` 归因。
4. 契约：§1 状态、§9 两行回填、§10 新增 D8/D9、§11 修订记录。

### 8.2 契约冲突（证据）

- **D8**：契约 §8 R3 期望第二次 host shutdown 在端点恢复后返回 `Complete` 或已完成移交的 `PersistedPending`。实测不可达：`peri-acp/src/host/telemetry_test.rs::host_shutdown_reenters_after_telemetry_pending` 断言第二次 `shutdown` 确实重入 `finish`（部署关闭权调用 2 次）、但交付尝试数不变（`endpoint.requests()` 前后相等）、返回值与第一次相同且非 Complete；根因是 `Batcher` 终态在 worker join 后冻结（`langfuse-client/src/batcher.rs::shutdown`：重复调用返回同一终态、不重新发送）。已按契约冲突登记进 §10 D8，待用户裁决。

### 8.3 跨流改动请求（本流不改代码）

1. **格式化门未通过**：`./scripts/cargo-sccache.sh fmt --all -- --check` EXIT=1，三处未格式化——`langfuse-client/src/batcher/admission_test.rs:225`、`langfuse-client/src/batcher/pending_test.rs:540`、`peri-tui/src/acp_client/deployment_test.rs:122`（本轮实测；属源码流，本流不代改）。
2. **过时归因与多余 allow**：`langfuse-client/src/spool/cleanup.rs` 文件头「生产 §5.2 移交分支未接线」与 `CleanupRegistration::disarm_after_handoff` 上的 `#[allow(dead_code)]` 已过时（调用点 `batcher/recovery.rs:314`，见 §7 更正）。
3. **文件规模超契约自设上限**（实测 `wc -l`；本变更集内没有超仓库硬上限 1000 行的文件——`scripts/check-file-size.sh` 报出的 3 个超限文件全为既有文件，但该脚本整体仍 EXIT=1；契约自设上限见契约 §7/§8，属源码流的拆分或契约裁决事项）：

| 文件 | 契约自设上限 | 实测 | 超出 |
| --- | --- | --- | --- |
| `langfuse-client/src/client_export_test.rs` | 900（测试） | 988 | +88 |
| `langfuse-client/src/batcher/recovery_test.rs` | 900（测试） | 957 | +57 |
| `langfuse-client/src/batcher/recovery.rs` | 500 | 579 | +79 |
| `langfuse-client/src/batcher/shutdown.rs` | 350 | 434 | +84 |
| `langfuse-client/src/batcher.rs` | 500 | 523 | +23 |
| `langfuse-client/src/batcher/worker.rs` | 650 | 664 | +14 |
| `langfuse-client/src/batcher/failure.rs` | 350 | 356 | +6 |
| `langfuse-client/src/client_send.rs` | 500 | 510 | +10 |
| `peri-controller/src/langfuse/tracer/input_snapshot.rs` | 400 | 488 | +88 |

注：任务简报给的 `recovery_test 957 / worker 655 / recovery 504 / failure 353` 与实测 `957 / 664 / 579 / 356` 在后三项不同（相关文件在 19:23–19:35 仍被集成流编辑），本表以本轮 `wc -l` 实测为准。已登记为契约 §10 D9（待裁决：维持现状或拆分）。

### 8.4 放行闸门

- 本轮实际写入 4 个文件（全部为文档/spec）：`docs/code-index/langfuse-client.md`、`docs/code-index/peri-controller.md`、`spec/issues/2026-10-12-langfuse-telemetry-spool-contract.md`、`spec/issues/2026-10-12-langfuse-telemetry-spool-report.md`；`docs/code-index/peri-config.md` 与 `docs/standards/{architecture-contracts,environment-variables}.md` 的改动来自更早的 P5 流，本轮只核对未改。未提交任何 commit，也未写任何 `.rs`/`Cargo.toml`。
- 本文与契约中的每个数字都来自 §2.1 的命令输出或 `wc -l`/脚本实测；未运行的项（真实端点、RSS、性能、clippy、E2E、目标宿主运行）继续保持「未测」。

## 9. 集成收敛轮（2026-10-12 20:00 起，收敛轮集成流）

### 9.1 输入状态与基线

- HEAD `607f8a222a4abaf37a7d3d68863cd244a1a37bb3`；`git status --short` 实测 66 个已跟踪文件改动 + 40 个未跟踪新文件（含本流新增 3 个源码文件）；`git diff --stat` 66 files changed, 6299 insertions(+), 1170 deletions(-)（含全工作树他流改动）。本流未 commit、未改冻结签名、未删或弱化任何测试。
- 本流开始前他流仍在写 `langfuse-client/tests/spool_recovery.rs`（19:56）、`spool_acceptance.rs`（20:05）；20:13 起这三个 `tests/*.rs` 只因本流 `cargo fmt -p langfuse-client` 变化（同一分钟批量 mtime：三个 test + `batcher/admission_test.rs` + `batcher/pending_test.rs`）。§9.3 的每条结果都对应 20:13 之后的磁盘内容。

### 9.2 按职责拆分（逐字搬移实现，不改行为、不改签名、调用点不变）

| 原文件 | 新文件 | 拆分依据 | 结果 |
| --- | --- | --- | --- |
| `batcher/recovery.rs` 579 | `batcher/recovery/scan.rs` 152 | 启动扫描 + 清理登记 ↔ 重试队列/送达 | 441（限额 500） |
| `batcher/failure.rs` 356 | `batcher/failure/barrier.rs` 75 | flush/shutdown 屏障账本 ↔ 交付计数账本 | 292（限额 350） |
| `batcher/worker.rs` 664 | `batcher/worker/recover.rs` 161 | 启动恢复接线 ↔ worker 主循环 | 521（限额 650） |

- 挂载沿用 crate 内既有 `foo.rs` + `foo/bar.rs` 约定（同 `spool.rs` + `spool/types.rs`），子模块由各自父文件挂载；**未改 `batcher.rs`**（其 523 行本身已超契约自设 500）。可见性沿用 `pub(in crate::batcher)` 与原 `pub(super)` 等价粒度，`recovery::*`/`failure::*` 路径由 `pub(in crate::batcher) use` 重导出保持调用点原样，离线 `cargo fmt` 后无 warning。
- `batcher/recovery_test.rs` 957/900 **未拆**：12 个用例共用同一份本地 `Server`+`Fixture` 夹具（同文件 100–433 行），拆出用例只能复制夹具或改共享夹具，而契约 §8 明令「不共享 test_helpers」，判定风险高于收益，留待裁决（§9.5）。

### 9.3 命令证据（本轮实跑）

| 命令 | 结果（输出末尾） |
| --- | --- |
| `./scripts/cargo-sccache.sh check --locked --workspace --all-targets` | `Finished dev profile ...`；EXIT=0（拆分后复跑） |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client`（连跑两次，拆分后） | 两次均 EXIT=0：`lib 282 passed; 0 failed; 1 ignored`、`tests/logging.rs 1`、`tests/otel_contract.rs 1`、`tests/spool_acceptance.rs 4`、`tests/spool_http.rs 3`、`tests/spool_recovery.rs 5 passed; 0 failed; 1 ignored`、doc-tests 0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-config` | `159 passed; 0 failed`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-controller` | `lib 180 passed; 0 failed`、`tests/langfuse_e2e.rs 5 passed; 0 failed`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-acp` | `lib 827 passed; 0 failed`、`compact_command_contract_test 2`、`concurrent_bg_agent_test 2`、`integration_test 5`、`prompt_cache_boundary 1`；EXIT=0 |
| `./scripts/cargo-sccache.sh fmt -p langfuse-client -p peri-config -p peri-controller -p peri-acp -- --check` | EXIT=0（本流格式化前曾 EXIT=1：`admission_test.rs:225`、`pending_test.rs:540`） |
| `./scripts/cargo-sccache.sh fmt --all -- --check` | EXIT=1：仅剩 `peri-tui/src/acp_client/deployment_test.rs:122`（不在本流 fmt 范围，见 §9.5） |
| `bash scripts/check-layer-imports.sh` | `§0 依赖门：共 22 条规则，违规边 0`；全边通过 |
| `bash scripts/check-file-size.sh` | 超 1000 行仍为既有 3 个文件（`peri-tui` 2 个 + `peri-resources/schema_test.rs` 1 个），本变更集无新增 |
| `git diff --check` | EXIT=0（无空白错误） |

### 9.4 仍红项（阻塞验收）

1. **`langfuse-client --test spool_recovery` 不稳定（复现率约 1/5，需上游修）**：验收要求「连跑两次稳定」，实测 19 次整文件复跑 4 次失败（21%）、30 次单用例复跑 1 次失败（超时）。三种表现同一根因（收敛轮二复核更正：`lock spool … ENOENT/EINVAL` 一类确为下述根因并已修复；`等待超时: 跨进程探针结论` 一类与锁无同源关系，见 §10.4 的排除实验）：
   - `list spool page: telemetry spool lock spool object failed: Invalid argument (os error 22)`（`spool_recovery.rs:352`，FAIL_12/25/30 命中 ENOENT/EINVAL）；
   - `等待超时: 跨进程探针结论`（`spool_recovery.rs:359`，探针子进程未写出 marker，20s 超时）；
   - 失败用例覆盖 `live_retry_wait_cannot_be_taken_over`、`corrupt_object_is_removed_without_send_and_next_round_recovers`。
   - 根因（本流实测）：`spool/native.rs::try_lock_file` 的 `options.open(lock_path)` 在「对象目录正被并发删除」时返回瞬态 `ENOENT`/`EINVAL`，`spool/native_scan.rs::list` 的「不可读条目」分支把它当硬错误 `?` 上抛，于是正常清理竞争升级为整页扫描失败（子进程探查同理 panic）。OS 级复现：macOS 8 线程复现（4 建锁文件 / 4 `rmtree` 同一目录）20s 内 `open(O_WRONLY|O_CREAT)` 报 EINVAL 159 次（其中 90% 伴随父目录已消失）、ENOENT 1037 次；不是测试环境特有，Linux 上等价表现为 ENOENT。
   - 生产影响：`batcher/worker.rs` 的启动恢复走同一 `list`，失败只 WARN 并跳过本轮恢复（`telemetry spool startup scan failed; disk state left as is`），不会丢正文，但整轮恢复被跳过。
2. **`batcher/recovery_test.rs` 957/900、`tests/spool_recovery.rs` 916/900** 仍超契约 §8 的测试文件自设上限（均低于仓库硬上限 1000）；前者理由见 §9.2，后者与 §9.4-1 的用例规模相关，交由裁决。
3. 本流未执行且仍为缺口的项：真实 Langfuse 端点、512 MiB 积压 RSS、性能测量、`cargo clippy -D warnings`、`e2e/`（与前几轮一致，不宣称产品验收完成）。

### 9.5 跨流改动请求（本流不改这些文件）

1. **native 端口删除竞争**（阻塞 §9.4-1，属 P1c/端口 owner）：`langfuse-client/src/spool/{native.rs,native_scan.rs,native_object.rs}`。建议把「锁文件所在对象目录已消失/正在被删除」与真实 IO 故障区分：`try_lock_file` 在 `open` 失败且父目录不存在（或错误为 `ENOENT`/`EINVAL`）时返回「不可领取」，`list` 跳过该条目、`claim` 归 `Ok(None)`、幂等 `delete` 不因此报错；并补一条「删除与 list/claim 并发不产生硬错误」的回归。验收判据：整文件 `spool_recovery` 连跑 20 次 0 失败。→ **已实现（收敛轮二，见 §10.1）**；判据改为「锁故障 20 次 0 失败」，另有 1 类探针超时见 §10.4。
2. **`cargo fmt -p peri-tui`**：`peri-tui/src/acp_client/deployment_test.rs:122` 未格式化，是 `fmt --all -- --check` EXIT=1 的唯一剩余项；本流 fmt 范围不含 peri-tui。
3. **其余超契约自设上限文件**（本流未动，实测 `wc -l`）：`client_export_test.rs` 988/900、`client_send.rs` 510/500、`batcher.rs` 523/500、`batcher/shutdown.rs` 434/350、`tracer/input_snapshot.rs` 488/400；对应契约 D9 的拆分或维持裁决。

## 10. 收敛轮二（2026-10-12 21:00 起：P6 收敛轮之后由主 agent 直接修复）

本节只记录**实际执行过的命令与实测数字**；与 §9 相同，未执行项一律标为缺口。

### 10.1 native 取锁竞争（§9.4-1 的阻塞项）修复

1. 根因：`spool/native.rs::try_lock_file` 的 `open(lock_path)` 在「锁文件父目录正被并发删除」时返回瞬态 `EINVAL`（macOS；Linux 为 `ENOENT`），`list`/`scan_totals` 的不可读条目分支把它当硬故障 `?` 上抛，正常清理竞争被升级为整页扫描失败（探针子进程同理 panic）。
2. 实现：`native.rs` 新增 `pub(crate) enum LockAttempt { Free(File), Held, Gone }` 与 `fn lock_parent_is_dir(path) -> bool`——`open` 失败且父目录不存在即 `Gone`，父目录仍在的失败照常上抛。10 个调用点按语义分流：`native_scan::list` 跳过该条目、`scan_totals` 的 `discard_entry` 直接 `Ok`、`native_object::{claim→Ok(None), delete_if_present→Ok(()), create→显式 NotFound}`、`publish` 的冲突仍按 `Corrupt`、`native_quota::{prune_stale 保守保留, reserve→显式 NotFound, sweep_writers 跳过, discard_without_quota→Ok}`。装配期目录缺失在**创建**路径仍按环境漂移显式报错，未放宽语义。
3. 回归：新增 `langfuse-client/src/spool/native_test/lock.rs`（188 行，由 `native_test.rs` 显式 `#[path]` 挂载）：`lock_attempt_reports_gone_instead_of_io_error_when_parent_is_deleted`（Free/Held/Gone 三态）、`concurrent_delete_never_turns_lock_attempts_into_io_faults`（竞争线程每轮新 ID 铺摘要不符条目再整目录删除 1.5s；断言 `list`/`claim` 取锁不得报 `Io{operation:"lock spool…"}`，且必须实际走到 `Corrupt` 持锁清理分支）。
4. 变异核验：把 `lock_parent_is_dir` 强制返回 `true`（等价回到修复前）→ 两条新用例全部失败并报 `Invalid argument (os error 22)`；还原后通过。

### 10.2 direct ingest 内存计量（P6 缺口①）修复

1. `snapshot.rs::measure_event` 提为 `pub(crate)`（唯一计量来源）、`memory.rs::GZIP_BILLING_BYTES` 提为 `pub(crate)`；`client.rs::direct_budget(events, config)` = Σ 逐条 `measure_event` + `max_wire_bytes` + 压缩时的 `GZIP_BILLING_BYTES`，全程 `checked_*`，溢出报 `LangfuseError::Config`。
2. `encode_direct` 改用它取代旧上界 `max_wire_bytes + limit.saturating_mul(2)`：旧式把「单条上限」乘 2 当成整批内存上界，与实际逐条计量无关，`limit` 放大即虚假放大边界。
3. 回归（`client_test.rs`）：`direct_budget_is_measured_not_multiplied`（含「放大 `max_request_bytes` 不改变上界」断言，直接暴露旧式）、`direct_ingest_fills_the_memory_prefix_without_budget_failure`。

### 10.3 能力探针竞争误判（本轮新发现，独立于 §9.4）

1. `spool/native.rs::probe_durable_io` 原把 `try_lock_exclusive()` 的**任何**失败都判为 `SpoolError::Unavailable`；对端（同部署另一进程）此刻持有 domain 锁时报 `WouldBlock`，于是 `open_at` 失败，`peri-acp/src/host/telemetry.rs::native_spool` 只 WARN 并**整次降级为有界内存模式**——而紧随其后的启动重建本会按 domain 锁排队完成。
2. 实现：探针只判能力——`Ok` 通过；`Err(WouldBlock)` 视为并发竞争放行；其余错误（如卷不支持 advisory lock）仍 `Unavailable`。
3. 回归：`native_test/lock.rs::peer_holding_the_domain_lock_is_contention_not_unavailability`。变异核验：去掉 `WouldBlock` 分支 → 该用例失败（`0 passed; 1 failed`）；还原后通过。

### 10.4 `spool_recovery` 稳定性复测与判定（P6 缺口②）

1. 修复后运行矩阵（均为 `./scripts/cargo-sccache.sh test --locked -p langfuse-client …`）：

| 跑法 | 轮数 | 结果 |
| --- | --- | --- |
| `--test spool_recovery`（整文件，`--test-threads=8`） | 20 | 20 ok（每轮 `5 passed; 0 failed`） |
| 同上 + 机器负载（8 路 CPU 忙转 + 2 路 `dd conv=fsync`） | 20 | 20 ok（每轮 `5 passed; 0 failed`） |
| `--test spool_http --test spool_acceptance` + 同负载 | 20 | 20 ok（每轮 `4 passed` + `3 passed`） |
| 单用例 `-- live_retry_wait_cannot_be_taken_over`（隔离；含锁修复，其中后 14 轮已带步进度插桩） | 30 | 30 ok |

2. 历史失败面（对照）：修复前 20 轮 @8 线程有 1 次 `list spool page: … lock spool object failed: Invalid argument (os error 22)`（已由 §10.1 关闭，并有变异核验）；修复后 20 轮整文件有 1 次 `等待超时: 跨进程探针结论`（20s 上限，子进程 stderr 为空 ⇒ 不是 panic），修复前单用例 30 轮另有 2 次同类超时。
3. 排除「domain 锁插入等待」：`acquire_domain_lock`（`native_quota.rs:60`）是全 spool 唯一的阻塞式加锁，若被反复插队即可解释 20s 超时。用裸 `flock(2)`（`fs2` 在 unix 走同一系统调用）实测：抢锁者连续 `LOCK_EX`+`LOCK_UN` 共约 2.25–2.53M 轮（blocking 与 spin 两变体各 3 次试验），**阻塞等待者的等待时间每次都是 0 ms**——macOS 会把锁让给等待者，不存在插队饿死；超时只能归因于「另一个真实进程完成启动+开库+领取」这一段 OS 级延迟（含调度与 fsync），不是锁语义缺陷。
4. 可观测性补齐（不放宽任何断言）：探针子进程把步进度逐行落盘到 `<marker>.steps`（Unix 毫秒 + `parent spawn → start → opened spool → listed → claimed`），父进程在超时那一刻即可断言卡在哪一步；子进程若提前退出，`wait_child_marker` 立刻带它的 stderr 失败，不再白等满 20s。
5. 判定：**修复后 90 轮（整文件 40 + 单用例 30 + http/acceptance 20）0 失败**，其中 40 轮在人为负载下；20s 上限保持不放宽（它覆盖的动作确实不受测试控制，但已有步进度证据可定位），因此不再宣称「删除竞争已完全稳定」之外更强的东西——剩余风险如实记为「并发抢锁跑法下偶发的进程启动延迟」，修复后未再复现。

### 10.5 命令证据（收敛轮二最终快照，2026-10-12 21:2x，机器空载）

| 命令 | 结果（输出末尾） |
| --- | --- |
| `./scripts/cargo-sccache.sh check --locked --workspace --all-targets` | `Finished dev profile … in 36.46s`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client` | lib `287 passed; 0 failed; 1 ignored`（22.71s）、`tests/logging.rs 1`、`tests/otel_contract.rs 1`、`tests/spool_acceptance.rs 4`、`tests/spool_http.rs 3`、`tests/spool_recovery.rs 5 passed; 0 failed; 1 ignored`、doc-tests 0；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-config --lib` | `159 passed; 0 failed`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-controller` | lib `180 passed; 0 failed`、`tests/langfuse_e2e.rs 5 passed; 0 failed`；EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p peri-acp --lib` | `827 passed; 0 failed`（99.02s）；EXIT=0 |
| `./scripts/cargo-sccache.sh fmt -p langfuse-client -p peri-config -p peri-controller -p peri-acp -- --check` | EXIT=0（首次复跑抓到本轮新红点 `langfuse-client/src/client.rs:358`，即 `direct_budget` 的手写换行；`cargo fmt -p langfuse-client` 后通过，仅该文件被改且只动空白） |
| `./scripts/cargo-sccache.sh fmt --all -- --check` | **EXIT=1**：唯一 diff `peri-tui/src/acp_client/deployment_test.rs:122`（跨流，见 §9.5-2，本流范围不含 peri-tui） |
| `bash scripts/check-file-size.sh` | EXIT=0；超 1000 行仍是既有 3 个文件（`peri-tui` 2 + `peri-resources/schema_test.rs` 1），本变更集 0 个 |
| `bash scripts/check-layer-imports.sh` | `§0 依赖门：共 22 条规则，违规边 0`；全边通过，EXIT=0 |
| `scripts/cargo-wasm.sh check --locked --target wasm32-unknown-emscripten -p peri-wasm --features cloudflare` | `Checking peri-wasm` → `Finished dev profile … in 20.06s`；EXIT=0（本轮改动后的平台编译面复核；闭包告警与 §4 登记一致，仍为 cfg 分支下的 dead_code/unused，不证明目标宿主运行行为） |
| `git diff --check` | EXIT=0（无空白错误） |
| `grep -rn "todo!\|unimplemented!"`（`langfuse-client/{src,tests}`、`peri-config/src`、`peri-controller/src/langfuse`、`peri-acp/src/host`） | 唯一命中既有 `peri-acp/src/host/executor_flow_parity_test.rs:24`（与本变更集无关，未改） |

注：§2.1/§9.3 记载的 `lib 282 passed` 是当日 19:2x 快照；收敛轮二实测为 287（本轮新增 `client_test.rs` 两条 direct-budget 用例与 `native_test/lock.rs` 一条能力探针用例，其余差值来自同一窗口内他流的补充用例）。

### 10.6 仍红项与缺口（收敛轮二结束时）

1. **自设行数上限**：`tests/spool_recovery.rs` 983/900（低于仓库硬上限 1000；较 §9.4 记载的 916 增加 67 行，来自探针步进度插桩）；`spool/native_test.rs` 由 971 拆为 841 + 新 `spool/native_test/lock.rs` 188，本变更集不再新增超自设上限文件。与 `batcher/recovery_test.rs` 957/900 及 5 个他流文件一并归入契约 D9，待裁决（维持或再拆）。
2. **`fmt --all -- --check` 仍 EXIT=1**：仅 `peri-tui/src/acp_client/deployment_test.rs:122`，属跨流改动请求（§9.5-2）。
3. **未测（与前几轮一致，不宣称产品验收完成）**：真实 Langfuse 端点的可查询性与服务端限额、1000 轮 8 MiB 长会话、512 MiB 积压与 32 MiB 遥测预算的实测 RSS、断网 10 分钟恢复、四持久边界崩溃注入的实机矩阵、WASM/无持久化宿主的运行行为、性能补表、`cargo clippy -D warnings`、`e2e/`。
4. **待用户裁决**：契约 §10 的 D8（第二次 shutdown 的期望被实测证伪）与 D9（自设行数上限维持或拆分）；本轮未改任何冻结签名、端口、owner 或计量口径，§10.3 的能力探针语义已同步契约 §3.2 与 `docs/standards/architecture-contracts.md`。

## 11. CI 修复（收敛轮三，PR #189 首次 CI 失败后由主 agent 修复）

本节只记录**实际执行过的命令与实测数字**；未执行项一律标为缺口。

### 11.1 失败面与根因

1. CI run `38331869301`（head `502d3d6f7`）三平台 job 全部失败、且同一用例：ubuntu `115083245535`、macos `115083245482`、windows `115083245618`，均在
   `langfuse-client/src/encoding/stream_test.rs:826` 的 `encoding::tests::frozen_baseline_bytes_are_stable` panic（`assertion left == right failed: none 编码必须逐字节等同旧格式`），job 退出码 100；`Classify changes` 与 `Verify test inventory` 通过。
2. 根因：`serde_json` 的对象底层随构建期 `preserve_order` feature 变化。CI 的 workspace 全量构建经 `agent-client-protocol`（peri-acp/peri-tui 依赖）启用该 feature（对象保插入序），本地单 crate `-p langfuse-client` 构建不启用（BTreeMap 字典序）：同一份事件因此编出不同字节。冻结格式要求逐字节可复现，旧期望值只在默认 feature 下成立。
3. CI 用 nextest 且默认 fail-fast：日志只到 `171/2160` 就停（三平台一致），因此首个失败掩盖了同类后续失败。本地按 CI 的 feature 并集复跑 `-p peri-controller -p peri-acp --lib` 抓到 peri-controller 5 个同根因失败（用例与位置见 §11.2-4）。

### 11.2 修复

1. 键序显式化：新增 `langfuse-client/src/stable_json.rs`（`StableJson`/`StableMap`/`Stable`，对象键递归字典序、数组保序）；`encoding/mod.rs` 的 `FrozenInput::Value`、`snapshot.rs` 的 `ValueSource::write_json`、`types/conversion/{metadata.rs,generation.rs}` 的 JSON 属性与 map/展开输出全部改经它。键序不再依赖编译图，输出与未启用 `preserve_order` 时的 `serde_json` 默认字节一致（即原 `none` 基准字节）。
2. 键序唯一实现：`snapshot.rs` 新增 `pub fn write_stable_json<W: io::Write + ?Sized>(out, value)`，`ValueSource::write_json` 与 controller 的 messages/tools 借用适配器（`peri-controller/src/langfuse/tracer/input_snapshot.rs::write_sorted_messages/sorted_tools`）都调用它。修前 fallback 路径逐元素 `to_value` + `serde_json::to_writer`，在 `preserve_order` 构建下会退回结构体声明序——与 raw 路径（已稳定化）在同一字段上产出两种口径，属同根因的第二处。
3. 测试期望同口径：`langfuse-client/tests/spool_acceptance.rs` 的 P5 期望字节由文件内 `stable_compact`（递归重排键）生成；`peri-controller` 三个测试文件（`bridge/snapshot_test.rs`、`tracer/input_snapshot_test.rs`、`tracer/production_observation_test.rs`）各加本地 `stable_bytes` 口径，`fallback_baseline` 与 5 处 raw 路径断言改用它。全部期望值都是**测试文件内独立实现**（不复用 `write_stable_json`），判据仍是逐字节相等，未放宽为解析等价。
4. 5 个 peri-controller 失败用例：`observe_raw_request_supersedes_snapshot_for_completed_generation`（`bridge/snapshot_test.rs`）、`request_end_cancel_order_holds_for_parallel_generations`、`cancel_keeps_terminal_once`（`tracer/input_snapshot_test.rs`）、`abandoned_generation_prefers_complete_raw_request`、`gated_generation_preserves_raw_request_through_late_subagent_join`（`tracer/production_observation_test.rs`）。
5. `cargo clippy --locked --workspace --all-targets -- -D warnings`（CI 同款入口）首次为红：`langfuse-client` lib 7 + lib test 14 共 21 个 error，逐项修复——
   `worker.rs` 误置 doc 注释（`empty_line_after_doc_comments`）、`worker.rs`/`recovery.rs` 的参数数（两处 `#[allow(clippy::too_many_arguments)]`）、`recovery.rs::expire_entry` 的 `result_large_err` 与 `batcher.rs` 的 `large_enum_variant`（均为「装箱只是搬家」的显式 allow）、`shutdown.rs` 的嵌套 if 改 match guard、`client_send.rs` 抽 `PendingCheckpointWrite` 类型别名、测试侧 4 处 `let`-返回（`Arc::<T>::clone(store)`）、`memory_test.rs` 无用 `as usize`、`native_test/lock.rs` 未显式 `truncate(false)`、`split_test.rs` 的 `&vec![..]`、`spool_http.rs` 的 `len() > 0`；`peri-controller` 1 个 error（`GenerationInput` 的 `large_enum_variant`，`Frozen(InputSource)` 208 字节，装箱只增加一层间接）同样显式 allow。**未改任何行为、冻结签名或判据。**
6. 复核：`-p langfuse-client --all-targets -- -D warnings` 已 EXIT=0；workspace 全量 clippy 复跑结论见 §11.4。

### 11.3 两个可疑偶发的判定

1. `langfuse-client/src/batcher/recovery_test.rs::recovery_deferral_keeps_objects_for_the_next_owner` 曾在本地 `-p langfuse-client -p peri-acp --lib` 单跑一次失败（`recovery_test.rs:812`：`fixed 页扫描跨多页：31 条`，断言要求 `recovered > 64`）。判定为负载相关的偶发，证据：CI 日志中该用例 `PASS [0.432s] (40/2160)`（三平台同）；随后单用例连跑 10 轮 10/10 通过（机器同时有其他 worktree 的编译负载，单轮 16–38s）。
2. `peri-resources` 的 `sessions::sqlite_store::connection::open_tests::incompatible_schema_is_rejected_without_wal_or_database_changes` 在 CI 等价 workspace 测试中失败过一次（`connection_open_test.rs:377` `session database initialization is busy`，既有文件、与本变更集无关），单用例复跑通过（`1 passed`）。同样判为偶发，未改该文件。

### 11.4 Windows 平台锁竞争与覆盖断言修复（收敛轮三续，2026-10-13）

1. 失败面：CI run `38336790068`（head `8f3688349`）的 Windows job `115098619419` 在 `Run workspace tests` 失败，后续步骤全 skipped，两条失败：
   - `langfuse-client spool::native::tests::claim_is_exclusive_across_processes`（`langfuse-client/src/spool/native_test.rs:509:42`）：`claim: Io { operation: "lock spool object", source: Os { code: 33, kind: Uncategorized, message: "The process cannot access the file because another process has locked a portion of the file." } }`——`claim` 返回了 `Ok(Err)`，而契约 §5.2/§8 要求对端持锁返回 `Ok(None)`。
   - `spool::native::tests::lock::concurrent_delete_never_turns_lock_attempts_into_io_faults`（`langfuse-client/src/spool/native_test/lock.rs:153:5`）：`扫描必须实际走到持锁清理分支（否则用例没有覆盖）`。
2. 根因（两处同源，均为「平台形态假设」）：
   - Windows 的 `LockFileEx` 在对端持锁时报 `ERROR_LOCK_VIOLATION`(33)，当前 Rust 的 `io::ErrorKind` 为 `Uncategorized` 而非 `WouldBlock`；`try_lock_file` 只按 `error.kind() == WouldBlock` 识别竞争（`native.rs`），Windows 因此落到 `Err(error)` 被包成 `Io { operation: "lock spool object" }`。契约语义（`Held` → `claim` 归 `Ok(None)`、`delete` 归 `Busy`；`probe_durable_io` 不降级为 `Unavailable`）本身没变，错的是判定形态。
   - 覆盖断言 `corrupt_pages > 0` 把「扫描走到持锁清理」的证据押在并发删除的产物上：Windows 上被占用句柄会使整目录 `remove_dir_all` 失败，摘要不符条目可能根本留不到扫描可见，该断言不可达——属用例自身的平台假设，不是产品缺陷。
3. 修复（只改判定形态与覆盖证据，语义与签名不动）：
   - `langfuse-client/src/spool/native.rs` 新增私有 `fn is_lock_contention(error: &io::Error) -> bool`：`error.kind() == io::ErrorKind::WouldBlock || CONTENTION_OS_ERROR.is_some_and(|code| error.raw_os_error() == Some(code))`，`CONTENTION_OS_ERROR` 用 `#[cfg(windows)] Some(33)` / `#[cfg(not(windows))] None`；`try_lock_file` 的 `Err` 分支与 `probe_durable_io` 都改用它（探针 doc 同步指向该函数）。`LockAttempt::{Free,Held,Gone}` 分流、`claim`/`delete`/`list` 的返回语义、端口与签名一律不变。
   - `spool/native_test/lock.rs`：把循环体内联的「写摘要不符条目」抽成 `write_corrupt_entry(...) -> bool`，在并发循环**之前**确定性铺一条（`id_of(0x5a)`）并断言 `list` 返回 `Err(SpoolError::Corrupt { .. })`——持锁清理分支由此获得与 OS 删除时序无关的证据；删除 `corrupt_pages` 计数与末尾 `corrupt_pages > 0` 断言。
   - 断言只增不减：循环内 `Err(Corrupt)` 仍计入 `pages`、「`list` 取锁不得报 IO 故障」（`is_lock_fault`）、`pages > 0`、「结束时无 IO 故障」全部保留，并新增 `Err(Corrupt)` 返回形态断言。用例名与文件位置不变。
4. 本机可验证面与缺口：Windows 错误码路径无法在 macOS 上运行。本机只做 `rustc --edition 2021 --target x86_64-pc-windows-msvc --emit=metadata` 的 `cfg(windows)` 分支编译检查（EXIT=0，同源片段）；真实行为由 Windows CI job 覆盖——`claim_is_exclusive_across_processes`（`claim` 走 `try_lock_file`）与装配期 `probe_durable_io` 都会命中该判定，回归即红。

### 11.5 命令证据（收敛轮三续）

| 命令 | 结果（输出末尾） |
| --- | --- |
| `./scripts/cargo-sccache.sh fmt --all` | EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client --lib spool::native` | `14 passed; 0 failed; 1 ignored`（2.05s；含 `claim_is_exclusive_across_processes`、`lock::concurrent_delete_...`、`lock::peer_holding_the_domain_lock_...`） |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client --all-targets` | EXIT=0：lib `292 passed; 0 failed; 1 ignored`（22.00s）、`logging` 1、`otel_contract` 1、`spool_acceptance` 4、`spool_http` 3、`spool_recovery` `5 passed; 0 failed; 1 ignored`（5.17s，直接覆盖跨进程取锁与删除竞争） |
| `./scripts/cargo-sccache.sh clippy --locked -p langfuse-client --all-targets -- -D warnings` | EXIT=0（6.41s） |
| `rustc --edition 2021 --target x86_64-pc-windows-msvc --emit=metadata`（`is_lock_contention` 同源片段） | EXIT=0（Windows `cfg` 分支可编译；仅编译检查，非运行验证） |
| `env -u PERI_WORKFLOW_ALLOW_NPX_FALLBACK ./scripts/cargo-rmcp-patched.sh test --locked --workspace --no-fail-fast --exclude peri-{acp,agent,tui,middlewares,mcp-common,mcp-web,mcp-artifact,mcp-cron,mcp-workspace}`（CI 的 workspace 步骤同口径 + 去本地噪声变量） | EXIT=0：44 个测试 target 全 ok，`2167 passed; 0 failed`（无 `test result: FAILED`） |
| `./scripts/cargo-rmcp-patched.sh test --locked -p peri-controller -p peri-acp --lib`（复刻 CI 的 feature 并集：`preserve_order` 生效，上一轮） | `peri-acp lib 827 passed; 0 failed`（134.80s）、`peri-controller lib 180 passed; 0 failed`（5.76s）；EXIT=0 |
| `./scripts/cargo-rmcp-patched.sh clippy --locked --workspace --all-targets -- -D warnings`（CI 同款入口，上一轮） | EXIT=0 |
| `./scripts/cargo-sccache.sh test --locked -p langfuse-client --lib -- recovery_deferral_keeps_objects_for_the_next_owner` × 10（偶发判定，上一轮） | 10/10 `1 passed; 0 failed`（单轮 16–38s） |
| CI run `38331869301` 三平台 job（上一轮失败面） | ubuntu `115083245535`、macos `115083245482`、windows `115083245618` 同一用例红；本轮修复后由 PR #189 新一轮矩阵复核 |
| 本轮 `git commit` 触发的 pre-commit（check / clippy / fmt / layer-imports） | 全绿（总 41.81s：check 27.06s、clippy 6.17s、fmt 6.75s、layer-imports 1.76s；依赖门 22 条规则 0 违规），提交 `f78b7a344` |

### 11.6 仍红项与缺口（收敛轮三续结束时）

1. **CI 的 workspace 步骤已在本机复跑通过**：`env -u PERI_WORKFLOW_ALLOW_NPX_FALLBACK ... --workspace --no-fail-fast --exclude peri-{acp,agent,tui,middlewares,mcp-common,mcp-web,mcp-artifact,mcp-cron,mcp-workspace}` → EXIT=0（44 target / 2167 passed，§11.5）。未被本机复跑的是 CI 的第二个测试步骤（per-acp/per-agent/TUI/middlewares/5 个 MCP 能力包）与三平台矩阵，交由远端 CI 覆盖；本变更集未改其源码。
2. **Windows 真实行为未经本机运行验证**：`ERROR_LOCK_VIOLATION`(33) 的判定只在 Windows 上生效，本机只做编译检查（§11.5 第 5 行）；结论以 Windows CI job 为准。
3. **本地环境噪声（非回归）**：本机会话环境带 `PERI_WORKFLOW_ALLOW_NPX_FALLBACK=1`（仓库与 CI 均无此变量，亦不在 shell profile），会让 `peri-workflow` 的三个 fail-closed 用例失守（`cli::tests::workflow_cli_fails_closed_without_any_validated_artifact`、`runner::artifact::tests::prepare_fails_closed_without_artifact_and_without_opt_in`、`tool::tests::failed_start_leaves_no_workflow_side_effects`）。证据：`env -u PERI_WORKFLOW_ALLOW_NPX_FALLBACK ... -p peri-workflow --lib` → `177 passed; 0 failed`，带该变量 → `3 failed`。该 workspace 等价全量跑走 `cargo test` 默认 fail-fast（第一个失败 target 之后不再启动后续 target），因此带该变量那次 `WS_TEST_EXIT=101` 只代表 `peri-workflow` 之前的目标已跑完；去掉该变量并按 `--no-fail-fast` 复跑即 EXIT=0（§11.5 末段）。
4. **待用户裁决不变**：契约 §10 的 D8（第二次 shutdown 的期望被实测证伪）与 D9（自设行数上限维持或拆分）。
5. **未测项不变**（与前几轮一致，不宣称产品验收完成）：真实 Langfuse 端点/服务端限额、1000 轮 8 MiB 长会话、512 MiB 积压与 32 MiB 遥测预算的实测 RSS、断网 10 分钟恢复、四持久边界崩溃注入的实机矩阵、WASM/无持久化宿主运行行为、性能补表、`e2e/`。
