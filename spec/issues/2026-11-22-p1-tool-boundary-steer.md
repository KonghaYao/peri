# P1：显式 steer 在工具批次中不再中断 turn，改为工具边界摄取

**状态**：已修复。
**优先级**：P1。
**类型**：行为变更（非缺陷）。
**创建日期**：2026-11-22。
**来源**：用户需求与逐项语义确认；主 agent 只读代码复核。
**确认方式**：用户确认投递/取消/预算/失败语义 + 主 agent 复核代码路径（基线 `1b7d3557b`）。本项不是缺陷回归，未另起 explorer 复核。

## 确认证据

- RCRA 循环为 `Receive → Compact → Reason → Act → 回 Receive`，Receive 是唯一消息消费点：`peri-agent/src/agent/stages/receive.rs:1-5`。
- Act 在模型响应含工具调用时整批并发执行并写入 transcript：`peri-agent/src/agent/stages/act.rs:137-180`、`peri-agent/src/agent/stages/tool_dispatch/execution.rs:654-707`。
- 显式发送当前会设置 `InterruptReason::Steer` 并取消当前 attempt：`peri-agent/src/session/user_input_mailbox/commands.rs:413`、`commands.rs:465`。
- 取消会打断 Reason/Act，并在 `finish_attempt` 后由新的 attempt 在入口 Receive 摄取；用户可见结果是当前工具批次被中止、turn 重启。
- MQ 的 `send_inputs` 只提交运行期发送选择；未领取内容仍由下一次 Receive 原子移交：`peri-acp-types/src/session/message_queue/message_queue.rs` 的 `accept_for_send` / `send_inputs`。
- 普通排队输入不会阻止 idle，只有已提交发送选择的用户输入会计入 `has_required_for_run`：`peri-acp-types/src/session/message_queue/message_queue.rs` 的选择逻辑。
- Act 之后 `has_tool_calls=true`，循环不会在 Receive 前退出；退出竞争由 `has_ensure_processing` continuation 兜底：`peri-agent/src/session/exec/executor_helpers/v2_execute.rs:677`。
- Compact 将 Human 作为 `user_requirements` 保护，摄取后的 steer 不会被摘要掉：`peri-agent/src/agent/compact_v2/planner/planning.rs:713-715`。

## 根因链

1. 用户在工具批次执行中点击“立即发送”。
2. `UserInputMailbox::dispatch` 接受发送选择后设置 `reason=Steer` 并取消 turn。
3. 取消 token 传到 Act 的工具批次，未完成的工具被中止；循环以 Interrupted 结束。
4. 宿主启动新的 attempt，入口 Receive 才把输入写入 transcript。
5. 结果：用户无法在当前 turn 的“下一个工具边界”纠正模型，必须经历一次重启；工具批次也可能产生部分副作用。

观察：上述 2-4 为代码路径事实；第 5 条为用户可观察影响。
推断：在长工具或工具审批等待中，这一重启会放大等待与浪费。

## 影响面

- 用户影响：TUI 待发送区的“立即发送”在模型流式输出时立即中断；在工具执行中也会中断整个 turn，而不是等当前工具批次完成。普通排队输入不受影响。
- 受影响模块/调用点：
  - `peri-agent/src/session/user_input_mailbox/`（dispatch、attempt 生命周期、阶段门）
  - `peri-agent/src/agent/stages/reason.rs`（实际 LLM 调用窗口）
  - `peri-acp/src/host/prompt/prompt_turn.rs`（steer interruption 的 activation suppress 路径）
  - 契约：`docs/design/user-input-queue.md`、`docs/design/thread-mq.md`
- 是否可规避：当前只能先 Stop 再发送，或等待 turn 结束；无法在同一 turn 的工具边界注入。

## 修复方案

- 范围：
  - 在 `MailboxState` 增加 `model_request_in_flight` 阶段门，由 Reason 的实际 `tokio::select!` LLM 调用持有 RAII guard。
  - `dispatch` 仅在阶段门为 true 时保持 `reason=Steer` + cancel；其余阶段只放行 MQ，不取消。
  - 不改 Receive 摄取逻辑，不新增 Ingest 阶段；工具批次完成后的下一次 Receive 即工具边界摄取点。
  - 保持 Stop 为唯一无条件硬中断；保持普通待发 idle-only。
  - 失败/Stop 在摄取前仍把未领取输入回收为 Queued。
  - 同一 Receive 批次按 MQ 顺序摄取多条 steer；预算仍消耗当前 turn 的语义迭代。
- 明确不做：
  - 不在每个工具返回后取消同批剩余调用。
  - 不让普通排队输入提前注入。
  - 不改变 goal/todo/后台完成等非用户来源的取消行为。
  - 不新增 UI 提示或新的待发送状态。
- 回归测试：
  - `peri-agent/src/session/user_input_mailbox_test.rs`
  - `peri-agent/src/session/user_input_mailbox_control_test.rs`
  - `peri-agent/src/agent/stages/loop_lifecycle_test.rs`
  - `peri-acp/src/host/steer_activation_test.rs`（流式窗口仍硬取消）

## 验收标准

1. 工具批次执行中点“立即发送”：当前批次全部完成，同一 attempt 的下一次 Receive 摄取该输入，下一轮 Reason 请求包含它；不出现 Interrupted。
2. 实际 LLM 调用/流式中点“立即发送”：仍立即取消当前 turn，新 attempt 入口 Receive 摄取。
3. Stop 在任何阶段都立即取消；未摄取输入回到 Queued。
4. 普通待发（未点“立即发送”）仍在 idle 边界投递，不进入工具边界注入。
5. 多条 steer 在同一批次按 MQ 顺序进入上下文；预算超限按现有错误结束，输入不丢。
6. 目标测试与 `cargo fmt --check`、定向 clippy 通过。

## 验证记录

基线 `1b7d3557b`，定向验证全部通过，详见
`docs/verification/steer-stage-gate-2026-11-22.md`：

- `peri-agent --lib user_input_mailbox`：60 passed / 0 failed。
- `peri-agent --lib loop_lifecycle`：7 passed / 0 failed（新增工具批次 steer 用例）。
- `peri-acp --lib steer_activation`：3 passed / 0 failed。
- `peri-tui --lib -- steer`：108 passed / 0 failed。
- `bash scripts/check-changed.sh`、定向 `clippy -D warnings`、`cargo fmt --check`、
  `git diff --check`：通过。

未覆盖：全量 `--all-targets`/CI 矩阵、真实 TUI 手工交互、外置 `peri-e2e` 场景；
阶段门边界竞态仅记录为行为边界，未做确定性复现。
