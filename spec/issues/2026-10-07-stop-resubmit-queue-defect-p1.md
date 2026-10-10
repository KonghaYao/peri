# Stop 后直接提交文本只入待发送缓冲区、不执行（P1）

**状态**：修复已实施（服务端语义），库测试与 E2E 通过；**待提交与用户现场验收**。修复前缺一次红灯 E2E 运行，证据构成见 [§复现与证据](#复现与证据)。行号以 2026-10-07 工作区为准。
**优先级**：P1。用户可稳定触发：取消后正常输入被静默吞掉，只能靠显式单发/整批（↑/⇈）或重新 loading 救回。
**类型**：会话控制 × 用户输入队列语义（服务端 staging 发布授权）。
**创建日期**：2026-10-07。
**来源**：用户现场提问「loading 中 cancel 掉之后，再发送文本，这个时候缓冲区的行为是不是有异常，会进入缓冲区并且不发送」。经代码复核确认缺陷成立，按用户裁决 **服务端语义修复** 落实为「新提交携带恢复授权」；同批补单测与 E2E 回归。

## 背景

- 队列语义：支持 user input queue 的客户端里，普通文本提交**一律**先入持久草稿（staging），不直接执行——`peri-tui/src/kit/submit_consumer.rs:152` 在 `supports_user_input_queue()` 时走 `steer_state::enqueue`，只有 remote command 例外。
- 发布语义：草稿在空闲时按 FIFO **逐条**发布，或由用户显式选择（单发/整批，`interrupt_current=true`）**整批原子**发布。loading 期间入队的内容因此天然停在「待发送」，这是设计行为，不是缺陷。
- Stop 语义：取消当前 attempt、保留待办、只回收明确未被 Receive 领取的内容，并把控制状态置为 Paused（`docs/design/user-input-queue.md`、`peri-acp-types/src/session_resources/control.rs:149-160`）。
- 缺陷范围：**Stop/Pause 之后的新提交**同样落入「待发送」，但没有任何自动路径再发布它。

## 缺陷链路（修复前）

1. **提交**：`peri-tui/src/kit/submit_consumer.rs:152` → `steer_state::enqueue`，只写草稿，不触发发布。
2. **取消**：`peri-tui/src/acp_client/client/requests.rs:275-282` 读控制态后二选一——有 `state.attempt` 发 `ControlAction::Stop { target }`，否则发 `ControlAction::Pause`。
3. **宿主**：`peri-acp/src/host/requests/session_control.rs:117-136` 对 Pause 与 Stop 都调用 `mailbox.stop()`；Stop 额外 `reclaim_unclaimed_durable`。
4. **邮箱暂停位**：`peri-agent/src/session/user_input_mailbox.rs` 的 `stop_matching` 置 `paused = true`、`reason = Stop`；`finish_attempt` 在 Stop 收尾时**不**解除该位。
5. **控制态**：Stop 使 `ControlStatus::Paused`（`peri-acp-types/src/session_resources/control.rs:149-160`）。
6. **发布准入被拦**：`peri-agent/src/session/user_input_mailbox/staging.rs` 的 `authorize_enqueue_publication` 在 `state.paused || control.status != Active` 时返回 `Ok(None)`，即**不给自动发布授权**；`PublicationBlock::detect`（同文件 `:21-33`）对 `state.paused` 与 `ControlStatus != Active` 同样直接判 `Paused` / `ControlInactive`。两条路径都堵死。
7. **唯一解阻路径**：显式单发/整批 → `dispatch_durable`，以 `interrupt_current=true` 走 work reducer，在 `peri-acp-types/src/session_resources/work/reducer.rs:292-308` 自动补发 `ControlAction::Resume`。

**结果**：停止后的普通提交拿到 `Queued` 回执后永远不会被发布，界面停在待发送缓冲区；用户必须再按一次 ↑ / ⇈ 才能发出。用户观察与代码一致。

## 设计契约冲突（修复前）

两份设计都规定「新提交可恢复」，实现只兑现了「立即发送」：

- `docs/design/user-input-queue.md:54`——「用户再次提交、显式继续或立即发送可以恢复停止后的处理。」
- `docs/design/rcra-message-activation.md:201`——「显式 Resume 或有明确恢复语义的新用户发送可解除暂停。」

即：缺陷不只是「少写了一个分支」，而是实现与既有设计契约不符。

## 复现与证据

- **静态链路**：上节第 1–7 步均为代码事实（含 `PublicationBlock::detect` 与 `authorize_enqueue_publication` 双重拦截），可独立复核。
- **临时复现测试**：修复前手工编写、验证后移除，确认「Stop 后 enqueue → 状态停留 Queued、无 delivery、无 MQ 消息」。
- **验证缺口（如实记录）**：**未**保留修复前的红灯 E2E 运行。E2E 只在修复后跑通，故 E2E 是**回归护栏**，不是缺陷存在的独立证据；缺陷存在的证据是上述静态链路 + 临时复现测试。
- **修复后回归**：新增 5 项单测（`staging_resume_test.rs`）与 1 项 E2E（`e2e/tests/smoke/steer-queue-live.test.ts`），覆盖「新提交自动恢复」「旧待办不自动发布」「收尾竞态」「授权一次性」四类语义。

## 修复语义（已实施）

**授权随「那条新提交」走，且为一次性，不回溯既有待办。**

- 暂停期间（或首次接纳时已处于暂停）提交的输入，在无活跃执行、控制态为 Active|Paused、且自身仍处于 `Queued` 时，登记一条 `resume_intents`（恢复授权）。
- 发布时按**新任务**语义发布：无 attempt 预期，由发布事务自动 Resume 并解除暂停位；不伪造 attempt、不推进控制代际以外的状态。
- 空闲发布若处于「恢复」模式，只从 `resume_intents` 命中的记录中取候选；**暂停之前入队的旧待办不因此自动发布**，仍按 idle FIFO 逐条交接。
- 授权在首次成功发布后消费，后续待办不得复用。
- 新提交**可能越过**暂停前的旧待办先发布——与既有「立即发送可越过」规则同构，属刻意取舍，已在设计文档写明。

**实现点**（`peri-agent/src/session/user_input_mailbox/staging.rs`）：

| 位置 | 作用 |
| --- | --- |
| `InputSelection::ResumeNewTask`（`:65-71`） | 新发布选择：固定无 attempt 预期，由发布事务自动 Resume |
| `enqueue_durable` 的 `resume_intent_candidate`（`:152-157`、`:257-260`） | 仅首次接纳时判定「暂停中提交」，发布成功后清 `state.paused` |
| 授权登记与即时发布（`:286-310`） | 记录 `resume_intents` 后**立即**尝试 `publish_next_durable()`；失败仅 `warn!("resume publication unconfirmed")`，授权与待发记录保留 |
| `authorize_enqueue_publication`（`:314-411`） | 准入放宽到 `Active | Paused`；`paused` 纳入判定，`automatic` 额外要求 `!paused` |
| `publish_next_durable`（`:475-528`） | 恢复模式跳过 `PublicationBlock::detect`、按 `resume_intents` 过滤候选、成功后清 `suspended`/`paused` 并移除授权 |
| `resuming_new_task`（`:530-551`） | 恢复前置条件：无活跃执行、无 attempt、状态 Active|Paused 且存在暂停位、授权非空、有匹配生命周期的 Queued 记录 |
| `publish_selection`（`:575`，`ResumeNewTask` 分支 `:670`）/ `project_staged_inputs`（`:705-721`） | `ResumeNewTask` 带 `interrupt_current=true`；发布/撤回后清理授权 |

邮箱状态新增 `resume_intents`（`peri-agent/src/session/user_input_mailbox.rs:106`，初始化 `:150`），语义在同处 doc comment 说明。

**测试**：

- `peri-agent/src/session/user_input_mailbox/staging_resume_test.rs`（新增，167 行）：
  - `explicit_selection_after_pause_resumes_atomically_without_fabricating_attempt`
  - `enqueue_after_stop_publishes_new_task_and_resumes`
  - `enqueue_during_stop_tail_publishes_after_attempt_finishes`
  - `queued_before_stop_is_not_auto_published`
  - `resume_authorization_is_consumed_by_first_publication`
- 原 `staging_test.rs` 中的暂停/恢复用例迁出，共享 helper 提升为 `pub(super)`（拆分原因为 `STD-SIZE-001` 单文件 1000 行上限）。
- E2E 新增「Stop 后直接提交新消息按新任务发送，无需显式单发」（`e2e/tests/smoke/steer-queue-live.test.ts`，文件共 6 例）。

**文档回写**：`docs/design/user-input-queue.md:54`、`docs/design/rcra-message-activation.md:201` 补写「恢复由该条新发送带动，暂停之前入队的待办不因此自动发布」；`docs/code-index/peri-agent.md` 两处（路由表「用户待发送、立即发送与停止恢复」行、§持久 RCRA 与 SDK 准入）同步。

## 影响面与不变项

- **未新增宿主唤醒接线**：停止收尾由 `peri-acp/src/host/user_input.rs` 的 `schedule_mailbox` 在 prompt 结束时接管；`enqueue_durable` 内的即时发布把「收尾恰好夹在写入前后」的窗口压到最小；TUI 侧 `peri-tui/src/kit/steer_consumer.rs` 的 5s `RECONCILE_INTERVAL` 仅作兜底。
- **未改**：`user_input_mailbox.rs` 中 `stop_matching`（`:536`）在活跃执行无 cancel token 时清零 `state.active`（`:559`）的旧行为（本次复核标记，未动，属独立候选）。
- **未改**：TUI `peri-tui/src/kit/steer_state.rs` 的 `direct_submissions` 显示细节（`7d475be7` 引入的短暂隐藏），可能是后续独立 issue。

## 验收标准

1. **主路径**：loading 中 Ctrl+C 取消后，直接输入文本并回车，消息按新任务发出且**恰好一次**（不重复、不丢）。
2. **不越权**：暂停之前入队的待办不因新提交被自动发布，仍在无显式选择时保持等待。
3. **不伪造执行身份**：恢复发布不产生伪造 attempt，控制代际推进方式与显式 Resume 一致。
4. **授权一次性**：一次恢复授权只兑现一条发布，不得被后续待办复用。
5. **竞态**：提交发生在停止收尾前后两个时序下都能收敛（`enqueue_during_stop_tail_publishes_after_attempt_finishes` 覆盖）。
6. **回归护栏**：新增 5 单测 + 1 E2E 全绿；`peri-agent` / `peri-acp` 库测试全绿。
7. **契约一致**：两份设计文档与实现语义一致，`docs/code-index/peri-agent.md` 已同步。

## 实施与验证记录（2026-10-07）

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| peri-agent 库测试 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib` | 1088 passed / 0 failed |
| peri-acp 库测试 | `./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib` | 793 passed / 0 failed |
| 邮箱专项 | `... -p peri-agent --lib -- user_input_mailbox` | 60 passed |
| E2E | `e2e/tests/smoke/steer-queue-live.test.ts` | 6/6 passed（新例约 2.3s） |
| 编译 | `... check --locked` | exit 0 |
| 静态 | `clippy -p peri-agent -- -W clippy::all`、`cargo fmt --check`、`typos` | 无新增问题 |
| 文件规模 | `scripts/check-file-size.sh` | 无新增超限（7 个既有超限测试文件未触及） |

**未做**：未提交（工作区保留全部改动）；未捕获修复前红灯 E2E；未做真机长时人工验收。
