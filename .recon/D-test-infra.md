# D-test-infra — Peri 构建/测试/验证基础设施地图

侦察对象：`/Users/konghayao/code/ai/peri-v4p3/.worktrees/peri-transcript-history`（分支 `feat/transcript-shrink-history-pagination`，HEAD `7f0de56e8`）。
目的：为后续阶段（transcript / history 分页相关回归测试）提供可执行的测试落点与命令。

---

## Summary

1. 所有 Cargo 命令必须经 `scripts/cargo-rmcp-patched.sh` 包装（`--locked`），裸 `cargo` 会因 `Cargo.lock` 记录本地 patched crate 而失败或改写锁文件（`patches/README.md:22-27,37`）。
2. 单测落点规则由 `docs/standards/testing.md:12-41` 定义：同目录 `_test.rs`（≥30 行）或文件末尾内联 `#[cfg(test)] mod tests`；`_test.rs` 必须在实现文件里用 `#[path]` 挂载，否则 `cargo test` 以 0 tests **假绿**退出。
3. compact 有专门的验证契约 `ARC-COMPACT-001`，其 Verify 行给出固定的测试过滤词清单（`docs/standards/architecture-contracts.md:72`），CI 另有独立步骤跑 4 套对抗套件（`.github/workflows/ci.yml:155-165`）。
4. session/load 的宿主集成测试集中在 `peri-acp/src/host/requests*_test.rs`，共享夹具是 `peri-acp/src/host/requests_test.rs`（MockTransport + make_server_config + HomeDirGuard），大量用例 `#[serial]`。
5. `peri-resources` SQLite 测试装在 `src/sessions/sqlite_store.rs` 的 `#[cfg(test)] #[path=...]` 块里，夹具是各测试文件内局部定义的 `make_store()` / `database()`（`tempfile::TempDir` + 真实 `SqliteThreadStore`）。
6. e2e 是 vitest + tmux 控制面（`e2e/scripts/run-e2e.mjs` + `config/tiers.mjs`），用例声明为 `e2e/tests/**/*.test.ts` 的 `describe/it`；文档口径在 `e2e/CLAUDE.md`，TUI 集成用例权威目录是 `docs/verification/tui-integration-cases/README.md`。
7. **不存在 Rust benchmark 基础设施**（无 `[[bench]]`、无 `benches/`、无 criterion）；性能验收是人工执行清单（`docs/verification/tui-integration-cases/09-performance.md`），且没有 e2e 用例引用该清单的编号。
8. `STD-SIZE-001`：源码与测试同为 **1000 行**上限，新增/修改文件必须满足；验收命令 `bash scripts/check-file-size.sh` + 对变更范围 `wc -l`。

---

## Findings

### 1. 构建/测试命令（含单测聚焦）

**包装脚本**：`scripts/cargo-rmcp-patched.sh`
- 固定下载 `rmcp 3.5.0`（`crate_url` / `crate_sha256` 常量，`scripts/cargo-rmcp-patched.sh:6-8`），校验 SHA-256、应用 `patches/rmcp-3.5.0-task-subscriptions.patch`，生成 gitignored 的 `target/peri-rmcp-patches/<hash>/cargo-config.toml`，再以该 `[patch.crates-io]` 配置执行传入的 cargo 子命令。
- 保留调用者工作目录（`patches/README.md:37`）。

**仓库根 `CLAUDE.md:64-68` 给出的 canonical 命令**：

```bash
./scripts/cargo-rmcp-patched.sh build --locked --workspace
./scripts/cargo-rmcp-patched.sh test --locked -p <crate> --lib -- <test_name>
lefthook run pre-commit
```

**`docs/standards/testing.md:303-336` 补充**（原文写裸 `cargo`，按根 `CLAUDE.md` 与 `patches/README.md:24-33` 应替换为脚本入口）：

```bash
cargo metadata --no-deps --format-version 1     # package 集合的事实源
cargo test --workspace                           # 全量
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo test -p peri-middlewares --lib
cargo test -p peri-tui --lib
cargo test -p peri-theme
cargo test -p <crate> --lib -- <test_name>       # 单测过滤
cargo test -p <crate> --test <target>            # crate 根 tests/ 集成目标
cargo test -p <crate> --test <target> -- --test-threads=1   # 改 env / 真子进程 / 真 wire
lefthook run pre-commit                          # fmt + check + clippy + layer-imports
```

**聚焦单个测试的正确姿势**（关键坑，`testing.md:31-41`）：
- 过滤词必须是**精确模块路径**。`mod tests;` 会让过滤词命中不到；过宽前缀会连带命中同目录其他模块。
- 例：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- session::transcript::projection_tests`。

**本次实测（已执行，非推测）**：上述命令于本 worktree 运行，完整输出存于 `.recon/test-run-projection.log`：
- 退出码 **0**；`Finished \`test\` profile [unoptimized + debuginfo] target(s) in 5m 18s`；`test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 1066 filtered out; finished in 0.03s`。
- 命中用例：`session::transcript::projection_tests::{test_legacy_compact_model_projection_preserves_storage, reminder_entry_preserves_stable_identity_without_placeholder_state, test_system_reminder_projects_once_as_human}`。
- 推论（可核对）：过滤词形态正确；`peri-agent --lib` 当前约 1069 个用例；该次运行曾因其他 cargo 进程占用 `target/` 构建目录锁而排队（见「Constraints & risks」第 10 条）。

**CI 执行面**（`.github/workflows/ci.yml`）：
- 构建：`build --locked --workspace --all-targets`（`:128-131`）。
- 测试：全部走 `nextest run --locked --workspace --no-tests=fail -E '<filter>'`，nextest 固定 `0.9.148`（`:113-118`），分 6 步：workspace（排除 ACP/Agent/TUI/middlewares/MCP 5 包）`:136-143`、peri-acp lib `:145-148`、peri-agent lib + 3 个 binary `:150-154`、**Compact 对抗套件** `:155-165`、peri-tui `:167-170`、middlewares lib `:172-176`、MCP 5 包 `:178-182`。
- doctest 单独一步（nextest 不支持 doctest）：`test --doc --locked --workspace --exclude ...`（`:184-195`）。
- 跳过规则集中在 `.config/nextest.toml` 的 `profile.default.default-filter`；串行靠 `serial-process-state` mutex 组（`max-threads = 1`），而非整步 `--test-threads=1`（`.config/nextest.toml:29-64`）。
- 清单一致性门：`scripts/ci-test-inventory.sh --check` 核对 ci.yml 过滤器与 nextest 配置（脚本头注释 `:1-25`）。

**pre-commit**（`lefthook.yml:5-40`）：`cargo fmt --check`、`check --locked`（全量）、`clippy`（仅本次改动命中的 crate，`scripts/precommit-changed-crates.sh`）、`check-layer-imports.sh`。注释明确：**doc tests 不在 pre-commit**，需显式 `test --locked --workspace --doc`（`lefthook.yml:36-37`）。

### 2. peri-agent 单测位置 + 3 个 transcript/compact 具体例子

**落点规则**（`docs/standards/testing.md:12-27`）：单元测试放同目录 `_test.rs`（测试代码 ≥30 行）或同文件末尾内联；集成测试放 crate 根 `tests/`（只能访问 `pub` API）；不设共享 `test_helpers` 模块。

peri-agent 现有 `_test.rs` 约 100 个，集成测试目标 8 个：`peri-agent/tests/{compact_authority_projection,compact_failure_adversarial,compact_followup_behavior,compact_pressure_adversarial,compact_session_adversarial,compact_session_manual_adversarial,stream_interruption_logs,stream_interruption}_test.rs`。`peri-agent/Cargo.toml` 无 `[[test]]` 显式声明 → cargo 自动发现。

**3 个具体例子**（均为单元测试，`#[path]` 挂载点已核实）：

1. `peri-agent/src/session/transcript_test.rs:196`（`#[tokio::test]` @195）
   `async fn test_commit_compaction_lifecycle_is_atomic_or_uncertain()`
   - 挂载：`peri-agent/src/session/transcript.rs:985-986` → `#[path = "transcript_test.rs"] mod tests;`
   - 过滤：`session::transcript::tests`
   - 断言对象：`MessageTranscript::commit_compaction_lifecycle(CompactionChange { flag_updates, appended_messages })` 的原子/不确定语义（成功才改内存；失败时磁盘事实保留、内存视图不前进、热态标记为不确定须冷重载）。这是 transcript×compact 交界处最直接的单元证据。
2. `peri-agent/src/session/transcript_projection_test.rs:57`（`#[test]`）
   `fn test_system_reminder_projects_once_as_human()`
   - 挂载：`peri-agent/src/session/transcript.rs:989-990` → `mod projection_tests;`
   - 过滤：`session::transcript::projection_tests`（已实测命中 3 例，见第 1 节）
   - 断言对象：system reminder 条目在模型视图里只投影一次，且投影为 human 身份。
3. `peri-agent/src/agent/compact_v2/full_test.rs:463`（`#[tokio::test]` @462）
   `async fn test_full_compact_sqlite_persists_lifecycle_and_preserves_ancestor_and_system()`
   - 挂载：`peri-agent/src/agent/compact_v2/full.rs:461-463` → `mod tests;`
   - 过滤：`agent::compact_v2::full::tests`
   - 夹具：`crate::session::test_resources::TestSession::open()`（真门面 + 真 SQLite + 真执行所有权，`full_test.rs:467`）。

**compact_v2 其余单元套件挂载点**（便于新增用例时选择归属）：
`peri-agent/src/agent/compact_v2/mod.rs:586-610`（`projection_tests`、`projection_render_tests`、`projection_restore_tests`、`projection_estimate_tests`、`projection_tool_use_tests`、`mod _test;`、`trigger_test`）、`planner.rs:370-372`、`micro.rs:59-61`、`summary.rs:207-209`、`config.rs:9-11`、`full.rs:465-471`（`report_tests`、`continuation_tests`）。

**共享夹具（与 testing.md「不共享原则」并存的既有例外）**：
- `peri-agent/src/session/test_resources.rs:17-24` `pub(crate) struct TestSession { resources, thread_id, .. }` / `TestSession::open()`，挂载于 `peri-agent/src/session/mod.rs:39-40` 的 `#[cfg(test)] #[path = "test_resources.rs"] pub(crate) mod test_resources;`。
- 该文件还带 `#[path = "test_resources/mock/mod.rs"] pub(crate) mod mock;`（`test_resources.rs:4-5`）。
- 部分集成测试不复用它：`peri-agent/tests/compact_authority_projection_test.rs:37-60` 的 `BoundSession::open()` 自行 `SessionResourcesImpl::open(...)`。

### 3. peri-acp 宿主集成测试位置 + session/load 现状

**两层入口**：
- 单元/宿主测试：`peri-acp/src/host/**/*_test.rs`，统一经 `#[cfg(test)] #[path = "..."] mod ...;` 挂载。主入口 `peri-acp/src/host/requests.rs:292-294`（`#[path = "requests_test.rs"] mod tests;`），该 `mod` 又用 `#[path]` 挂入 20+ 子测试文件（`peri-acp/src/host/requests_test.rs:28-40`、`:558-609`）。
- crate 集成测试（只走 pub API）：`peri-acp/tests/{compact_command_contract_test.rs, concurrent_bg_agent_test.rs, integration_test.rs, prompt_cache_boundary.rs}`。

**共享夹具（`peri-acp/src/host/requests_test.rs`）**：
- `MockTransport`（`:49`，记录 notifications）、`make_provider_config(:85)`、`make_peri_config_with_provider(:105)`、`make_server_config(:112)`、`make_server_config_with_bridge(:129)`、`build_server_config(:142)`、`create_bound_fixture(:217)`、`register_session_with_history(:300)`、`restore_env_var(:392)`（HOME 守卫）。
- 真门面 + 裸桥配对打开走 `peri_resources::sessions::open_store_and_facade_for_tests`（`requests_test.rs:137`）。

**session/load 实现入口**：`peri-acp/src/host/requests/session_restore.rs`（`load_session_snapshot` :54、`load_session_payloads` :138/:147、`load_session_binding` :352、`load_session_meta` :356/:439、`load_session_history` :462、history replay 失败降级 :525）；路由分流 `peri-acp/src/host/server_loop.rs:573-582`（`"session/load" | "session/resume"`）。

**今天怎么测 session/load（具体函数名）**：

| 文件 | 测试函数（行号） |
| --- | --- |
| `peri-acp/src/host/requests_history_lifecycle_test.rs` | `test_clean_load_resume_and_fork_ignore_caller_cwd_and_keep_saved_facts` (:101, `#[serial]` @100)、`load_and_resume_after_runtime_loss_preserve_session_history` (:155)、`another_host_handle_can_restore_without_peri_execution_exclusion` (:198)、`persisted_closing_intent_blocks_restore_until_close_drains` (:281)、`read_only_store_keeps_history_readable_but_rejects_load_and_close_mutations` (:330)、`unloaded_close_and_delete_settle_normally` (:238) |
| `peri-acp/src/host/requests_frozen_cases_test.rs` | `test_session_load_cold_host_restores_original_frozen_prompt` (:86, `#[serial]`)、`test_session_load_future_frozen_snapshot_fails_without_overwrite` (:310) |
| `peri-acp/src/host/requests_legacy_test.rs` | `legacy_history_context_then_load_restores_saved_cwd_and_frozen_snapshot` (:39, `#[serial]`)、另有 11 条 legacy adoption / 只读 / drain 用例（:157 :202 :250 :296 :334 :441 :510 :628 :722 :791 :852） |
| `peri-acp/src/host/requests_mcp_cases_test.rs` | `test_session_load_prewarms_mcp_discovery_smoke` (:177) |
| `peri-acp/src/host/requests_beta_flags_test.rs` | `restored_session_keeps_frozen_beta_flag` (:171) |
| `peri-acp/src/host/requests_workspace_cases_test.rs` | `worktree_binding_hot_cold_resume_ignore_caller_cwd_and_keep_saved_facts` (:13)、`cold_load_derives_builtin_closed_set_from_persisted_frozen` (:675) |
| `peri-acp/src/host/requests_workflow_cases_test.rs` | `session_new_and_load_publish_task_snapshots_and_revisioned_changes` (:5) |
| `peri-acp/src/host/requests/session_lifecycle_replay_test.rs` | `test_compact_reminder_replay_mpsc_wire` (:33)、`test_compact_reminder_replay_stdio_wire` (:72) |
| `peri-acp/src/host/stdio/run_server_integration_test.rs` | `test_initialize_and_session_new_over_stdio_transport` (:413)、`test_prompt_wire_shape_unknown_session_returns_error` (:516)；`run_server_session_resources_test.rs:99` 用 `load_session_snapshot` |
| `peri-acp/src/host/requests_lifecycle_cases_test.rs` | delete/rename 生命周期（:7 :73 :100 :134 :198 :230） |
| `peri-acp/src/host/session_io_test.rs`（挂在 `user_input_test.rs:5-6`） | `session_io_preparation_rejects_mutations_during_close_but_allows_snapshot` (:4)、`removed_execution_protocols_are_unsupported` (:27) |

**夹具形态**（`requests_history_lifecycle_test.rs:5-95`）：`binding_state()` / `frozen_state()` 直读门面；`struct Fixture { cfg, bridge, tmp, cwd, id, sessions, transport }`，`Fixture::new()` 经 `handle_request("session/new", ...)` 建会话后用裸 `SqliteThreadStore` 追加历史，`assert_restored_history()` 断言恢复后的 cwd / 首条消息 / frozen。

**compact 经宿主（含 load 后手动 compact）**：`peri-acp/src/host/compact_history_fixture_test.rs:18-19` 挂 `#[path = "compact_command_test.rs"] mod compact_command_tests;`，整文件又挂在 `peri-acp/src/host/executor_flow_test.rs:799-800`（`#[cfg(not(windows))]`），模块路径为 `host::executor_flow_tests::compact_history_tests::compact_command_tests`；用例：`test_manual_compact_twice_through_host_preserves_canonical_history`（`compact_command_test.rs:62`）、`test_auto_full_then_manual_compact_through_host`（:101）、`test_cold_snapshot_then_manual_compact_through_host`（:120）、`test_manual_compact_model_cancel_has_consistent_wire_and_preserves_history`（:146）。

**相关契约要求的过滤词**：`ARC-COMPACT-001` Verify 含 `cargo test -p peri-acp --lib compact_report`、`cargo test -p peri-acp --lib compact_recovery`（`docs/standards/architecture-contracts.md:72`；`docs/code-index/peri-acp.md:69-70` 指向 `host/prompt.rs` + `session/command/compact_{persistence,report}_test.rs` + `tests/compact_command_contract_test.rs`）。

**`#[serial]` 的读侧闭合要求**（真实事故记录）：`peri-acp/CLAUDE.md:64`、`:72`——触碰 builtin `workspace` 资源面的用例依赖 `PERI_MCP_BUILTIN` 默认态，而该进程级 env 由开关组用例在 `#[serial]` 临界区内改写，读侧必须同键 `#[serial]`，否则资源面静默缺席（断言拿到 `None`/空）。同处记录 `-p peri-acp --lib` 基线 **775 passed / 0 failed**（`:72`）。

### 4. peri-resources SQLite 测试位置 + 夹具模式

**位置与挂载**：
- 根 store 测试：`peri-resources/src/sessions/sqlite_store.rs:615-617` → `#[path = "sqlite_store_test.rs"] mod tests;`
- 同文件后续挂载（`sqlite_store.rs:619-645`）：`read_only_test.rs`（`mod read_only_tests`）、`sqlite_inherited_context_test.rs`、`sqlite_store/legacy_test.rs`、`sqlite_store/session_data_test.rs`（`mod session_data_tests`）、`sqlite_store/thread_child_delete_test.rs`、`sqlite_store/schema_v10_test.rs`（`mod schema_v10_tests`）等。
- 二级挂载：`sqlite_store_test.rs:100-101` → `frozen_snapshot_tests`；`sqlite_store/session_data_test.rs:855` → `mod history_tests;`（`session_history_test.rs`）；`sqlite_store/schema.rs:582` → `schema_test.rs`。
- crate 集成契约测试：`peri-resources/tests/session_resources_contract.rs`（只看 `SessionResources` 公共行为，文件头注释 `:1-4`）。
- 其他：`peri-resources/src/sessions/{resources,sqlite_store,storage_v11_migration,storage_v2_plan,schema_shape,default_path,open,machine,context}_test.rs`；远端面在 `src/sessions/remote/*_test.rs`。

**夹具模式（每个测试文件内局部定义，符合 testing.md:206-213）**：
- `peri-resources/src/sessions/sqlite_store_test.rs:12-18`
  `async fn make_store() -> (SqliteThreadStore, tempfile::TempDir)` → `SqliteThreadStore::new(dir.path().join("test.db"))`。
  另有 `#[cfg(test)] fn absolute_test_path(relative: &str) -> String`（`sqlite_store.rs:607-612`）。
- `peri-resources/src/sessions/sqlite_store/session_data_test.rs:20-26`
  `async fn database() -> (SqliteThreadStore, SqliteSessionData, TempDir)`，配套 `frozen(marker)`（:29）、`binding_of`、`workspace(store, cwd)`（:37）、`session(id, cwd, workspace, frozen)`（:41）、`payloads(count)`（:63）、`payload_bytes`。
- 配对打开（门面 + 裸桥）：`peri-resources/src/sessions/mod.rs:104-107`
  `pub async fn open_store_and_facade_for_tests(db_path: impl Into<PathBuf>) -> anyhow::Result<(SqliteThreadStore, SessionResourcesImpl)>`，注释明确「生产装配一律走 `open_facade`，本函数仅测试夹具」——它**不是** `#[cfg(test)]`，可被其他 crate 的测试引用（peri-acp `requests_test.rs:137`）。
- 真 git 仓库夹具：`peri-resources/tests/session_resources_contract.rs:28-45` 的 `fn git(root, args)`，用 `env_clear()` + `GIT_CONFIG_NOSYSTEM=1` 隔离。

**典型行为用例（history/compact 相关，可直接参照）**：`sqlite_store/session_history_test.rs:4` `test_projection_and_compaction_maintain_derived_views_in_the_same_write`、`:131` `test_rewind_boundaries_are_distinct_and_unknown_cutoffs_change_nothing`、`:443` `test_restart_preserves_canonical_snapshot_and_history`；`sqlite_store_test.rs:25` `test_sqlite_store_flush_persistence_makes_messages_and_flags_readable`。

### 5. e2e 测试框架：声明、运行、文档

**声明**：`e2e/tests/**/*.test.ts`，vitest 的 `describe` / `it`（示例 `e2e/tests/scenarios/compact-command.test.ts:9-19`）。配置 `e2e/vitest.config.ts`：`fileParallelism: false`、`maxConcurrency: 1`、`testTimeout: 600_000`、`hookTimeout: 120_000`、`setupFiles: ["./tests/setup.ts"]`、`globalTeardown: ["./scripts/generate-report.ts"]`、`env.PROJECT_ROOT`。
目录：`tests/{smoke,scenarios,panels,subagent,tool-cards,workflow}`。

**运行**（`e2e/package.json:7-15` 与 `e2e/CLAUDE.md:11-24`）：

| 场景 | 命令 |
| --- | --- |
| 改单个用例 | `npm run e2e -- --file tests/<path>.test.ts --serial --retry 0` |
| PR / 日常冒烟 | `npm run e2e:l0` |
| 合并前 | `npm run e2e:l1` |
| 发版全量 | `npm run e2e:release` |
| 发版且不容忍首轮 flake | `npm run e2e:release:strict` |
| 本地单文件不走控制面 | `npm test -- tests/<path>.test.ts`（`e2e/CLAUDE.md:24`） |

数据流（`e2e/CLAUDE.md:28-31`）：`run-e2e.mjs → vitest worker → helpers/peri.ts → dev.sh → Peri TUI (tmux) → summary.json / report.md / recordings/`。控制面 `e2e/scripts/run-e2e.mjs` 每个 worker 独立 vitest 进程并注入 `E2E_RECORDINGS_DIR` / `E2E_PARALLEL=1`（`:363-392`），结果写 `results/run-*/summary.json`（`:879`，含 `flake.firstAttemptFailed`）。
分层定义 `e2e/config/tiers.mjs:13-60`：l0 显式文件清单（含 `tests/scenarios/legacy-history-upgrade.test.ts`，`:19`）、l1 按目录 `smoke/panels/tool-cards`、l2/release 全量（parallel 3 / retry 1 / maxFirstAttemptFailures 2）；`resolveTierFiles` 在 `:63-80`。
已废弃：`npm run e2e -- --all`、`e2e:all`（`e2e/CLAUDE.md:22`）。

**helpers**（`e2e/CLAUDE.md:37-50`）：`peri.ts`（`launchPeri` :329、`sendPrompt` :347、`takePeriSnapshot` :385、`launchPeriHITL` :404、`waitForStableScreen` :428；`PROJECT_ROOT` :19、`DEV_SH` :20）、`build.ts`（`buildPeriForE2e()` → `scripts/cargo-rmcp-patched.sh build --locked -p peri-tui --bin peri`）、`stdio-acp-fixture.ts:226`（`startStdioAcpFixture`，真 `peri acp --cwd` JSONL peer，要求隔离绝对 HOME）、`replay-model.ts`（`startReplayModelServer` :136、`launchReplayTui` :232、`REPLAY_PERI_BIN = target/debug/peri` :21）、`workflow.ts`、`judge.ts:260`（`judge()`，需 `OPENAI_API_KEY`/`JUDGE_MODEL`，见 `e2e/.env.example`）、`recorder.ts`、`bg-task-boundary.ts`、`workspace-mcp-fixture.ts`。

**稳定不变量**（`e2e/CLAUDE.md:52-61`）：等待 UI 用独有文本；`sendKey` 用 tmux 键名；`waitForStableScreen` 先变化再连续 3 次稳定；Judge 仅在 `expect(result.pass)` 时阻断；完成态优先磁盘因果；**L0 不得依赖真实模型/凭据/外部网络（TEST-HERMETIC-001）**（`:59`）。

**文档层级**：运行口径 `e2e/CLAUDE.md`；测试规范 `docs/standards/testing.md:339-352`（E2E 命令只在 e2e 指引维护）；TUI 用户可观察验收用例的**权威目录** `docs/verification/tui-integration-cases/README.md`（编号 P0-01…P0-76，区域文件 `01-session.md`…`15-browse-layout.md`，逐条链接 active issue；执行口径要求固定二进制/终端尺寸、同时核对屏幕与请求/事件/持久数据）。

**与后续阶段最相关的既有 e2e 用例**：
- `e2e/tests/scenarios/legacy-history-upgrade.test.ts`：用 `node:sqlite` 的 `DatabaseSync`（`:7`、`:56`）手工造旧 schema 库，`CURRENT_SCHEMA_VERSION` 从 `peri-resources/src/sessions/canonical.rs` 正则解析（`:18-26`），`beforeAll` 先 `buildPeriForE2e()`（`:38`），不使用模型/judge —— 这是「真实旧库 → ACP → TUI」的现成模板。
- `e2e/tests/scenarios/compact-command.test.ts`（/compact 手动压缩 UI 不崩）、`thread-switch.test.ts`、`rewind-v2.test.ts`、`clear-chat.test.ts`。

**本 worktree 运行 e2e 的前置条件（已核实为缺失）**：`e2e/node_modules` **不存在**（需 `npm install`）；`e2e/tui-tester` 是 git submodule 且**目录为空**（`.gitmodules:4-6`，需 `git submodule update --init e2e/tui-tester`）。本机可用：`tmux 3.6a`、`node v26.9.0`、`cargo 1.99.0`。

### 6. 性能 / benchmark 基础设施

- **无 Rust bench 设施**：全仓库 Cargo.toml 无 `[[bench]]`，无 `benches/` 目录，无 `criterion` 依赖（`grep -rn "criterion|\[\[bench\]\]|benches/" --include=Cargo.toml` 命中 0）。CI 中 `bench` 仅作为 `--all-targets` 的构建覆盖出现（`.github/workflows/ci.yml:120` 注释），不执行。
- **无 e2e 性能门**：`e2e/config/tiers.mjs` 只定义功能分层；`grep -rn "P0-41|P0-42|P0-43|09-performance" e2e/tests e2e/helpers` 命中 **0** —— 性能清单的编号没有任何自动化用例引用。
- **人工性能验收清单**：`docs/verification/tui-integration-cases/09-performance.md`（P0-41 长历史淘汰缓存后仍可操作、P0-42 长流式与多子任务保持响应、P0-43 长任务后新请求不随历史显著退化、P2-02 大历史预览切换释放派生内容）。口径：「固定二进制、终端尺寸和负载，记录 CPU 时间、延迟、RSS 与缓存计数；不得用单次进程百分比替代差分证据」。
- **有界的计时型回归测试**（是断言上界，不是 benchmark）：
  - `peri-resources/src/sessions/sqlite_store/read_only_test.rs:538-548`：`Instant::now()` 测量 `readonly_error_kind` 耗时并断言分类为 `DatabaseUnreadable`。
  - `peri-resources/src/sessions/sqlite_store/discovery_test.rs:216-235`：`test_worktree_hanging_git_ends_within_the_call_budget`，打印实测并断言 `measured >= 5s`（单次预算）。
  - `peri-resources/src/sessions/remote/cloud_limit_test.rs:269-275`：`append_ms` 实测打印。
- **产品内延迟指标**（非测试）：`host/diagnostics.rs::ResponseDiagnostics` 的 `perf.input` 记录 RPC/session/command 身份与响应发送完成耗时（`docs/code-index/peri-acp.md:9`）——可作为 load 路径观测点，但目前没有测试消费它做性能门。

### 7. STD-SIZE-001 约束摘要

来源 `docs/standards/index.md:39-43`：
- **Scope**：仓库源码及测试文件（`.rs`、`.ts`、`.tsx`、`.mjs`、`.js`），扫描遵循 `.gitignore` 并排除构建产物目录。
- **Rule**：单个文件最多 **1000 行**（含空行与注释），**源码与测试使用相同上限**。新增或修改文件须满足限制；既有未改动超限文件的治理另立任务，不要求在无关任务中重构。拆分按职责和模块边界，不以机械搬移或压缩排版规避。
- **Verify**：`bash scripts/check-file-size.sh` 全量列出超限文件；对本次变更范围用 `wc -l <changed-source-paths>` 核对。全量扫描中的存量超限须如实报告，**不宣称全库通过**；自定义扫描阈值不替代验收上限。

脚本实现 `scripts/check-file-size.sh`：默认 `MIN=1000` / `TEST_MIN=1000`（`:39-40`），按后缀 `*_test.rs|*_test.ts|*_test.tsx|*/_test/*|*/tests/*` 分类源码/测试（`:78-88`），任一超限 exit 1。
存量基线：`peri-acp/CLAUDE.md:72` 记录最近一次 `check-file-size.sh` 为 `EXIT=1`（源码 3 / 测试 29，均为存量，未在本波修复）。
给新代码的可操作含义：单个新测试文件不要超过 1000 行；现有大测试文件（`peri-agent/tests/compact_failure_adversarial_test.rs` 939 行、`peri-agent/src/agent/compact_v2/planner_test.rs` 961 行）已接近上限，扩写时须先按职责拆分。

---

## Constraints & risks

1. **假绿风险（最高）**：`_test.rs` 未挂载 / 过滤词写错时 `cargo test` 以 **0 tests 退出 0**（`docs/standards/testing.md:31-41`）。新增测试必须同时确认挂载点与过滤词命中非零用例（CI 靠 `--no-tests=fail` 兜底，本地裸 `cargo test` 无此保护）。
2. **验证证据要求 TEST-EVIDENCE-001**（`testing.md:184-188`）：结论必须来自最终退出状态与可核对结果；不得用未启用 `pipefail` 的 `| tail` / `| grep` 作为通过证据；须报告精确命令、exit status、实际执行用例数或结构化 verdict。
3. **TEST-HERMETIC-001**（`testing.md:190-194`）：访问 HOME / cache / 配置 / 凭据 / 时间 / 网络 / 进程全局状态的测试必须用临时目录与固定时钟；改写进程级 env 必须隔离并串行化（`#[serial]`）。peri-acp 侧有真实事故：读侧漏标 `#[serial]` 导致资源面静默缺席（`peri-acp/CLAUDE.md:64,72`）。
4. **TEST-LIFECYCLE-001**（`testing.md:196-200`）：持久缓存/恢复类集成测试必须跨真实生命周期（cold fetch → 写盘 → 进程退出 → 新进程 warm hit → invalidate 回源）；单进程 unit test 或 ignored test 不能替代。history/load 相关改动应覆盖进程边界。
5. **fixture 不共享原则与既成例外并存**：`testing.md:206-213` 要求所有 mock/fixture 在测试文件内局部定义，禁 `mockall`/`Mock struct`/`#[cfg(feature = "test-utils")]`；但仓库已有 `peri-agent/src/session/test_resources.rs`（`#[cfg(test)]`）与 `peri_resources::sessions::open_store_and_facade_for_tests`（非 cfg(test)，注释声明测试专用）两个跨文件夹具入口。新增夹具应沿用局部定义，避免扩大这两处。
6. **compact 改动有强制验证清单**：`ARC-COMPACT-001` Verify（`docs/standards/architecture-contracts.md:72`）列出 14 个过滤词/测试目标；CI 有独立步骤跑 4 套对抗套件并要求每套命中非零用例（`.github/workflows/ci.yml:155-165`）。
7. **load 相关另有契约**：`ARC-SESSION-LOAD-001` Verify（`architecture-contracts.md:78`）绑定 `cargo test -p peri-tui --lib load_reservation` 等 4 个 peri-tui lib 过滤词 + 人工检查 `ThreadLoadDispatcher::send`、`AcpTuiClient::open_prompt_after_session_loads`。现有实现/测试点：`peri-tui/src/acp_client/client_reverse_test.rs:516/576/618`、`peri-tui/src/kit/thread_load_consumer_test.rs:110`、`peri-tui/src/kit/acp_events_test/session_events_test.rs:771`。
8. **数据库表结构变更需用户明确批准**（根 `CLAUDE.md:35`，核心工程原则 12）——若 history 分页需要 schema 改动，须先说明表及结构合理性并获得裁决。
9. **构建/运行成本**：`target/` 有部分缓存（约 315M），本次实测仍在 5m18s 内完成 `-p peri-agent --lib` 的编译+运行；e2e 需要 `npm install` + submodule init + 每次 `buildPeriForE2e()` 构建 `peri-tui`（`e2e/helpers/build.ts`，超时 600s）。
10. **Cargo 构建目录锁**：同一 worktree 内并发 cargo 调用会在 `target/` 上串行等待（本次实测聚焦测试命令曾长时间卡在 `Blocking waiting for file lock on build directory`，同时段存在其他 cargo/rustc 进程）。并行跑多个测试命令不会更快。
11. **Peri loader 不继承父目录 `CLAUDE.md`**（根 `CLAUDE.md:44`）：跨层改动需显式读 `peri-agent/CLAUDE.md`、`peri-acp/CLAUDE.md` 与相关 code-index。

---

## Open questions

1. **本次 history 分页/transcript 收缩的目标契约在哪？** 在 `spec/issues/`、`spec/global/`、`docs/design/` 与 `architecture-contracts.md` 中检索 `pagination|分页|shrink|增量加载` 未找到与该分支（`feat/transcript-shrink-history-pagination`）对应的 active spec 或 ARC-xxx 条目；`architecture-contracts.md:5-193` 的契约列表中也没有 history/pagination 条目。需要确认新契约编号、Scope 与 Verify 应落在哪份文档，否则「按 STD-INDEX-002 区分实现缺陷 / 文档过时 / 契约变更」无法裁决。
2. **分页是否需要碰 schema？** 若要新增表/列，需按根 `CLAUDE.md:35` 先获得用户批准；相应测试会落在 `peri-resources/src/sessions/sqlite_store/schema_v*_test.rs` 与 `schema_shape` / `storage_v11_migration` 系列（挂载与夹具模式见第 4 节）。
3. **e2e 是否需要为该特性增加 L0 用例？** 现有 L0 清单（`e2e/config/tiers.mjs:13-25`）没有 history 分页用例；若新增，须满足 L0 hermetic 要求（隔离 HOME + 本地假 model server，`e2e/CLAUDE.md:59`），否则放 L1 及以后。
4. **性能验收的载体**：没有 bench 设施，`09-performance.md` 是人工清单且无自动化引用。若后续阶段需要可回归的性能证据，需先决定是补 Cargo bench、还是把该清单的采集口径固化成带阈值断言的测试——当前仓库两者都没有先例。
5. **`target/` 锁竞争的应对**：本次侦察期间同一 worktree 上有其他 cargo 进程，聚焦测试命令被构建目录锁阻塞数分钟。若后续多个阶段并行开发，是否需要为每个 worktree 独立 `CARGO_TARGET_DIR` 以避免排队，需要确认。
6. **`e2e/tui-tester` submodule 与本地工具链**：该 submodule 未初始化、`e2e/node_modules` 缺失，且 submodule URL 是 `git@github.com:`（需 SSH 凭据）。若后续阶段要跑 e2e，需要确认这些前置条件由谁准备。
