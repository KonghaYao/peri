# P1：子 Agent 模型失败后无法 resume

**状态**：已修复，定向验证通过；全 workspace 测试交由 CI。
**优先级**：P1。
**类型**：错误。
**创建日期**：2026-10-27。
**来源**：https://github.com/KonghaYao/peri/issues/297 。
**确认方式**：explorer 只读确认 + 主 agent 复核（基线 `3f5d2b091`）；已通过 sync/background 真实 factory 的确定性测试复现并修复。

## 修复前基线证据

- issue 报告模型 TLS 连接重试耗尽后，resume 返回 `child resource owner is not available for resume`。
- `peri-agent/src/session/subagent/resource_owner.rs:131-139`：closing、preparing、active、failure 共用上述报错。
- `peri-agent/src/session/subagent/run_sync.rs:697-712`：Failed 且有未结义务时将执行 permit 以 Err 结算；background 存在同构判断。
- `resource_owner.rs:169-179`：未结义务包含 active task、pending delivery 和 Required 消息，不能据此判断可靠交接失败。
- `resource_owner.rs:457-479`：permit 的 Err 或未结算 Drop 留下 failure；后续 resume/acquire 拒绝 failure。
- 既有已批准契约 `2026-10-20-p1-sync-subagent-unbounded-settlement.md:22-29` 要求可靠交接后释放计算、保留任务与消息，并由显式 resume 再次推理。

## 根因链

模型执行失败且保留任务/消息 → 即使交接已经可靠提交，sync/background wrapper 仍按动态未结义务设置 owner.failure → 同进程后续 resume 永久拒绝。

上述修复前代码路径及此类缺陷已由确定性红绿回归验证；现场日志未包含 owner 的具体阻塞字段，故现场命中 failure 而非 closing/active 仍是推断。

## 影响面

- 用户影响：同一 child 在错误后无法继续，错误提示无法区分阻塞原因。
- 受影响模块：peri-agent 子 Agent sync/background 执行结算及 owner resume 准入。
- 规避：进程重启后可从持久历史创建新执行，但不会恢复旧后台执行，不能作为保留后台任务的修复方案。
- 定级：核心续聊路径错误；历史仍保留且可重新加载，不属于已确认数据丢失或全局核心流程不可用。

## 修复方案

- 区分执行失败与资源交接失败：可靠交接成功的 Failed outcome 不因存在待处理任务/消息而毒化 owner。
- 保留真实持久化失败、未确认交接、活跃执行及关闭中的拒绝保护。
- 改善 resume 准入拒绝原因，不新增持久状态或数据库结构。
- 回归覆盖 sync/background 模型失败后同一 child 的恢复、原 manager/queue 复用，以及真实结算失败仍被拒绝。
- 明确不做：TLS 网络重试策略、进程重启恢复旧执行、无关 close 生命周期重构。

## 验收标准

1. 确定性复现修复前失败、修复后通过；不依赖真实 TLS 服务。
2. 已可靠交接的模型失败后，同一 child 可 resume，保留原后台任务与消息，不重复注册任务。
3. 未确认持久化/交接失败仍阻止恢复；active/closing 准入不被削弱。
4. 目标测试、format、check-changed、git diff --check 通过；未完成验证明确记录。

## 实施结果

1. `Failed { residual: Some(receipt) }` 的 receipt 已经 canonical 确认，sync/background 不再因运行中任务、待投递或 Required 消息而将 owner 标记为失败。
2. history flush 单独追踪；交接写入、终态持久化或事件排空失败继续阻断。`Failed { residual: None }` 在 permit 结算时仍检查动态未结义务；没有义务时允许显式新执行。
3. `prepare_resume`、`acquire`、`check_resume` 在同一把状态锁内共用准入判断，区分 closing、preparing、active、pending handoff 与未确认结算，不把内部错误内容返回给调用方。

## 验证记录

命令均在指定 worktree 执行；Cargo 使用 worktree 内独立 `CARGO_HOME`、`TMPDIR` 和 `CARGO_NET_OFFLINE=true`。模型错误由 fixture 构造，不连接 TLS 服务。

1. 修复前红灯：`./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- test_failed_model_handoff`，执行 2 项、均失败、exit 101；`test_failed_model_handoff_sync_can_resume_original_owner` 和 `test_failed_model_handoff_background_can_resume_original_owner` 均在真实 factory 的后续 resume 准入处失败。
2. 修复前诊断红灯：同一命令入口过滤 `test_resume_rejects_unconfirmed_settlement_with_specific_diagnostic`，执行 1 项、失败、exit 101，原错误为 `child resource owner is not available for resume`。首次使用错误的完整过滤路径得到 0 tests，未作为红绿证据。
3. 修复后目标测试：以下命令均 exit 0，实际运行总计 73 项：
   - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- session::subagent::tests::resume_owner_reuse::`：15 项，包括上述 3 个红灯及 sync/background 交接后 flush/事件排空失败、未确认交接、无 receipt 有/无剩余义务场景。
   - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- session::subagent::resource_owner::tests::`：13 项，包括准备/执行/关闭互斥、pending receipt 和并发准备。
   - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- session::subagent::run_sync::tests::`：11 项。
   - `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- session::subagent::child_runner::tests::`：34 项，包括后台事件排空失败与本机受控进程组用例。
4. `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --doc`：15 项通过，exit 0。
5. `cargo fmt --all -- --check`、`bash scripts/check-changed.sh`、`git diff --check`：均 exit 0；check-changed 覆盖 peri-agent 及其反向依赖闭包。
6. `bash scripts/verify-async.sh read`：exit 4，无已有回执；未启动异步全量验证。全 workspace 测试及 CI 尚未执行，不以定向测试替代。
7. `bash scripts/check-file-size.sh`：exit 1，扫描发现 20 个既有超限文件（源码 8、测试 12），均不在本次修改中。本次修改的 Rust 文件均不超过 1000 行，复审补测后最大为 `resume_owner_reuse_test.rs` 的 985 行。
8. issue 和新增测试文件是未跟踪文件，不包含在普通 `git diff --check` 中；额外执行 `git diff --no-index --check -- /dev/null <issue>` 无空白错误输出，但因文件不同返回 1，不能记为通过。随后对未跟踪文件单独检查尾随空白，结果通过。

## 独立验收

- 独立 verification 去重通过 233 项：peri-agent 189 项、peri-model retry 29 项、doc tests 15 项；另有定向用例重复运行，未重复计数。
- peri-agent build、Clippy `--lib -- -W clippy::all`、format、check-changed 与 diff 空白检查通过；不是全 workspace `--all-targets -D warnings` 验收。
- 二次独立 review 确认 mailbox 阻塞关闭，本次限定范围无阻塞。
- 未覆盖真实 TLS 到 resume 的端到端及独立进程重启；测试以模型错误 fixture 和无 owner 的冷恢复模拟验证。完整 workspace 测试交由 CI。
- 非阻塞缺口：注册失败测试覆盖 acquire 前失败，尚未直接断言 acquire 后注册失败的 mailbox 状态，以及后台执行门放行与 handoff 确认失败的交错。

## 独立 review：同进程 mailbox 责任

1. **关联与契约**：本修复允许存在运行中任务 B 的已确认失败交接继续 resume；如果任务 A 已经 Receive 并落 canonical、模型失败后进入 Blocked，A 属于 Full Compact 必须保护的会话级责任。原 factory 虽复用 manager/MQ，却重新创建 mailbox，因而此前被阻断的 resume 现会暴露责任丢失。
2. **红灯**：修改 owner 前运行 `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib -- test_failed_child_resume_preserves_blocked_result_through_full_compact`，1 项失败、exit 101；真实 factory 后台 resume 在 Reason 前实际 Full，观察到 `summary_contains=true, excluded=true, reason_contains=false`。任务 A 已由 Receive 规范提交并处于 Blocked，任务 B 保持 active 且 handoff 已确认。
3. **中间检查**：仅复用 mailbox 后，原测试错误地在真实任务完成后要求 registry 仍保留任务，1 项失败、exit 101；修正为完成前断言恰好一个任务、完成后断言已结清。随后补充执行开始时的状态断言，1 项失败、exit 101：`Blocked` 未转为 `Ready`。
4. **实现与绿灯**：`ParkedChildOwner` 保留同一进程的 mailbox；factory 与旧任务完成回调用同一实例。同步获得 permit/claim、后台注册及认领移交后才执行 `resume_scope`，准备失败不改变 Blocked。`resume_preserves_blocked_result_through_full_compact` 实际运行 4 项、exit 0，覆盖首次 sync/background 失败 × 二次 sync/background resume；断言 Full 不归档 A、主模型看到 A、旧任务 B 完成投递且重复投递不重复入队、旧计算 session 被释放。
5. **补充边界**：`test_cold_resume_does_not_reconstruct_mailbox_or_tasks` 和 `test_failed_resume_registration_keeps_mailbox_blocked` 各 1 项通过；`confirmed_handoff_terminal_write_failure_blocks_resume` 的 sync/background 2 项通过。冷恢复仅加载历史，不恢复旧执行、任务或 mailbox；未新增 DB 或持久状态。
6. **最终目标验证**：同上 Cargo 环境下依次过滤 `session::subagent::tests::resume_owner_reuse::`（23 项）、`session::subagent::resource_owner::tests::`（13 项）、`session::subagent::run_sync::tests::`（11 项）、`session::subagent::child_runner::tests::`（34 项）、`session::subagent::tests::resume_cases::`（20 项）、`session::subagent::tests::resume_dispatch_cases::`（9 项）、`session::async_result_mailbox::tests::`（17 项）、`agent::compact_v2::full::protected_result_tests::`（5 项），共 132 项，均 exit 0。`./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --doc` 15 项通过，exit 0。
7. **最终检查**：`cargo fmt --all -- --check`、`bash scripts/check-changed.sh`、`git diff --check` 均 exit 0；`bash scripts/check-file-size.sh` 仍因 20 个既有超限文件 exit 1。`bash scripts/verify-async.sh read` exit 4，无回执；未启动异步任务，CI/全 workspace 测试未运行。
