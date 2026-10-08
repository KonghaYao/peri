# 任务 A：执行恢复剥离交付记录

日期：2026-10-07。工作树：`/Users/konghayao/code/ai/peri-remove-execution-recovery-20261007`。
仅修改 `peri-acp-types/`、`peri-resources/`；未提交，未操作真实用户数据库、SDK 源码或原脏工作树。
授权与总验收以 `spec/issues/2026-10-07-remove-execution-recovery-plan.md` 为准。

## 接口决策

- 删除 `session_resources::{work, control}`、`execution_admission` 及恢复专用 SessionResources/SessionDataPort 方法、SQL adapter 和 gate 路径；不留 shim。
- `ToolContext` 删除 `invocation_intent`、`invocation_work_target`、`session_lifecycle` 与 `with_work_invocation`；保留当前 invocation/session/cancellation/session_resources。
- 新增 `pub tool_call_id: Option<String>`，构造默认 None；`with_tool_call_id(impl Into<String>)` 独立设置当前模型卡片身份，不混用 invocation ID。
- 删除恢复专用 `InvocationTargetMetadata`/`BaseTool::invocation_target`、`CronContinuationRequest::recipient_control`、队列回执 `work_receipts`/`publication_generations` 与 `PeriCaps::session_recovery_v1`。
- 删除冷恢复 MCP 端口 `cold_session_tools`、`recover_workspace_tasks` 和已无生产消费者的持久 lifecycle binding 方法；保留普通 session binding、当前任务 watch、普通关闭校验。
- 无纯运行期 ControlAttempt 用例需要迁移；没有保留控制 reducer/journal 或恢复 main 的 execution lease/dirty。
- 保留普通历史、flags/projection、frozen/inherited、关闭意图、事务/错误/未决写门禁及 Turso `peri_op_ledger` 普通写入确认。

## 迁移决策

- 两端 schema 同步推进 18；新库只建九张业务表，远端另保留两张机制表。
- 17→18 只执行六表删除与版本推进，不重建九张业务表；删除前共享预检精确识别基线六表定义，拒绝同名未知表/视图及未知附属或外部 view/trigger/FK/index 引用。
- 六表合法形状的 DDL 仅作为迁移比对常量与测试夹具，不作为生产新库建表计划执行。
- 本地在 BEGIN IMMEDIATE 中读取 schema、预检、删除和推进版本；未知结构拒绝时对象、行与版本不变。
- 远端一致读取 schema 后，在同一托管事务内守卫对象数量、每个对象 SQL、版本、store_id 与 contract，再删除和推进版本。
- 远端晚到的 schema 变化、身份变化及版本变化不能绕过守卫；丢响应或不完整提交确认继续返回 PersistenceUncertain/Unknown，不能当成功。
- 旧版本升级保留 thread_goals 与其未知扩展，不恢复 execution_runs；11→12 对 threads 原位补列和填充归属，避免旧重建丢失扩展列、索引、trigger 或 goal 引用。
- 回归核对 messages.rowid、普通历史追加/重开、业务表 rootpage、goal 数据及扩展对象。

## 验证

所有 Cargo 命令前缀：`./scripts/cargo-rmcp-patched.sh`；使用 `--locked --offline`，shell 为 login=false。

| 命令参数 | 结果 |
| --- | --- |
| `test --locked --offline -p peri-acp-types --lib` | 526 passed |
| `test --locked --offline -p peri-resources --lib schema -- --test-threads=1` | 83 passed |
| `test --locked --offline -p peri-resources --lib history -- --test-threads=1` | 41 passed，1 ignored |
| `test --locked --offline -p peri-resources --lib storage_v2_migration -- --test-threads=1` | 3 passed |
| `test --locked --offline -p peri-resources --test session_resources_contract` | 6 passed |
| `test --locked --offline -p peri-acp-types --doc` | 1 passed，2 ignored |
| `test --locked --offline -p peri-resources --doc` | 0 tests，成功 |

定向 check 已成功；最终源码由上述测试重新编译。未跑 workspace 全量 test/clippy 或 peri-tui 入口 check。
Rustfmt 经 stdout 计算差异并用 apply_patch 落盘；本范围 diff 空白检查、修改文件 ≤1000 行校验及恢复接口残留扫描通过。
日志在 `/tmp/peri-a-{types,schema,history,storage,contract,doc}-test.log`；ignored 项不算通过。

## 未闭合与边界

- A 范围没有已知未闭合实现问题；跨层消费者与 peri-tui 集成验证由协调者及 B/C/D 完成，本记录不宣称全工作区通过。
- 真实 Turso 网络、WASM 与 UI E2E 未执行；远端迁移 guard、失败回滚和提交 Unknown 通过 SQLite-backed RemoteTransport 定向验证。
- 未知六表形状或外部依赖会明确拒绝迁移，不自动删除用户扩展数据。识别基线形状不代表接受任意第三方变体。
- code-index 与跨目录权威文档由协调者同步，本 worker 不越过目录所有权。

## 修改文件

M = 修改，D = 删除，A = 新增。以下清单仅包含任务 A 两个目录。

- `M` `peri-acp-types/src/cron.rs`
- `D` `peri-acp-types/src/execution_admission.rs`
- `D` `peri-acp-types/src/execution_admission_test.rs`
- `M` `peri-acp-types/src/lib.rs`
- `M` `peri-acp-types/src/peri_caps.rs`
- `M` `peri-acp-types/src/peri_caps_test.rs`
- `M` `peri-acp-types/src/ports.rs`
- `M` `peri-acp-types/src/session/user_input.rs`
- `M` `peri-acp-types/src/session/user_input_test.rs`
- `M` `peri-acp-types/src/session_resources.rs`
- `D` `peri-acp-types/src/session_resources/control.rs`
- `D` `peri-acp-types/src/session_resources/control_test.rs`
- `D` `peri-acp-types/src/session_resources/work.rs`
- `D` `peri-acp-types/src/session_resources/work/admission.rs`
- `D` `peri-acp-types/src/session_resources/work/availability.rs`
- `D` `peri-acp-types/src/session_resources/work/bindings.rs`
- `D` `peri-acp-types/src/session_resources/work/bindings_test.rs`
- `D` `peri-acp-types/src/session_resources/work/command.rs`
- `D` `peri-acp-types/src/session_resources/work/command_test.rs`
- `D` `peri-acp-types/src/session_resources/work/delivery.rs`
- `D` `peri-acp-types/src/session_resources/work/delivery_query_test.rs`
- `D` `peri-acp-types/src/session_resources/work/policy.rs`
- `D` `peri-acp-types/src/session_resources/work/policy_test.rs`
- `D` `peri-acp-types/src/session_resources/work/processing.rs`
- `D` `peri-acp-types/src/session_resources/work/processing_test.rs`
- `D` `peri-acp-types/src/session_resources/work/projection.rs`
- `D` `peri-acp-types/src/session_resources/work/query.rs`
- `D` `peri-acp-types/src/session_resources/work/reducer.rs`
- `D` `peri-acp-types/src/session_resources/work/terminal_query_test.rs`
- `D` `peri-acp-types/src/session_resources/work/user_input.rs`
- `D` `peri-acp-types/src/session_resources/work/user_input_test.rs`
- `M` `peri-acp-types/src/tools.rs`
- `M` `peri-acp-types/src/tools_test.rs`
- `D` `peri-acp-types/tests/perf_probe.rs`
- `M` `peri-resources/src/sessions/canonical.rs`
- `D` `peri-resources/src/sessions/control.rs`
- `M` `peri-resources/src/sessions/data.rs`
- `M` `peri-resources/src/sessions/mod.rs`
- `M` `peri-resources/src/sessions/remote/full_remote_test.rs`
- `M` `peri-resources/src/sessions/remote/mod.rs`
- `M` `peri-resources/src/sessions/remote/schema.rs`
- `M` `peri-resources/src/sessions/remote/schema_upgrade.rs`
- `M` `peri-resources/src/sessions/remote/schema_upgrade_test.rs`
- `M` `peri-resources/src/sessions/remote/schema_v12_upgrade.rs`
- `M` `peri-resources/src/sessions/remote/schema_v14_upgrade.rs`
- `D` `peri-resources/src/sessions/remote/session_control.rs`
- `D` `peri-resources/src/sessions/remote/session_control_test.rs`
- `M` `peri-resources/src/sessions/remote/session_data.rs`
- `D` `peri-resources/src/sessions/remote/session_delivery_query_test.rs`
- `D` `peri-resources/src/sessions/remote/session_pending_work_test.rs`
- `M` `peri-resources/src/sessions/remote/session_schema.rs`
- `M` `peri-resources/src/sessions/remote/session_shape_test.rs`
- `D` `peri-resources/src/sessions/remote/session_work.rs`
- `D` `peri-resources/src/sessions/remote/session_work_journal.rs`
- `D` `peri-resources/src/sessions/remote/session_work_test.rs`
- `M` `peri-resources/src/sessions/resources.rs`
- `D` `peri-resources/src/sessions/resources/control.rs`
- `M` `peri-resources/src/sessions/resources/gate.rs`
- `D` `peri-resources/src/sessions/resources/gate_control.rs`
- `D` `peri-resources/src/sessions/resources/gate_work.rs`
- `D` `peri-resources/src/sessions/resources/work.rs`
- `D` `peri-resources/src/sessions/resources_pending_work_test.rs`
- `M` `peri-resources/src/sessions/resources_test.rs`
- `M` `peri-resources/src/sessions/schema_cleanup.rs`
- `M` `peri-resources/src/sessions/sqlite_store/schema.rs`
- `M` `peri-resources/src/sessions/sqlite_store/schema_cleanup.rs`
- `M` `peri-resources/src/sessions/sqlite_store/schema_test.rs`
- `M` `peri-resources/src/sessions/sqlite_store/schema_v10_test.rs`
- `M` `peri-resources/src/sessions/sqlite_store/schema_v11_test.rs`
- `M` `peri-resources/src/sessions/sqlite_store/schema_v7_test.rs`
- `M` `peri-resources/src/sessions/sqlite_store/session_data.rs`
- `D` `peri-resources/src/sessions/sqlite_store/session_data/control.rs`
- `D` `peri-resources/src/sessions/sqlite_store/session_data/work.rs`
- `M` `peri-resources/src/sessions/storage_v2_migration.rs`
- `M` `peri-resources/src/sessions/storage_v2_migration_test.rs`
- `D` `peri-resources/src/sessions/work.rs`
- `D` `peri-resources/src/sessions/work/availability.rs`
- `D` `peri-resources/src/sessions/work/diagnostics.rs`
- `D` `peri-resources/src/sessions/work/effects.rs`
- `D` `peri-resources/src/sessions/work/effects_test.rs`
- `D` `peri-resources/src/sessions/work/prepared_effects_test.rs`
- `D` `peri-resources/tests/durable_work/delivery_query_contract.rs`
- `D` `peri-resources/tests/durable_work/journal_contract.rs`
- `D` `peri-resources/tests/durable_work/lifecycle_contract.rs`
- `D` `peri-resources/tests/durable_work/reopen_contract.rs`
- `D` `peri-resources/tests/durable_work/request_retention_contract.rs`
- `D` `peri-resources/tests/durable_work/terminal_contract.rs`
- `D` `peri-resources/tests/durable_work_contract.rs`
- `D` `peri-resources/tests/durable_work_http_contract.rs`
- `D` `peri-resources/tests/session_control_contract.rs`
- `A` `peri-resources/src/sessions/remote/schema_v18_test.rs`
- `A` `peri-resources/src/sessions/sqlite_store/schema_v18_test.rs`
- `A` `peri-resources/execution-recovery-removal-report.md`
