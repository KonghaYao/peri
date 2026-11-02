# P1：暂存区消息在 turn 进行中点击发送后没有发出

状态：已确认，修复随本分支提交。日期：2026-11-02。
基线：`origin/feat/thread-mq` @ `6e41c9ebb`（PR #321）。缺陷与回归用例依赖该分支的
Thread MQ 持久收件箱；`origin/main` 缺少行级发送状态，用例前置条件不成立。

## 现象

正在流式输出回复 A 时，在用户消息暂存区输入 B 并单击 B 行上的 `↑` 发送：

1. A 被中断（符合预期）；
2. B 离开暂存区后没有产生任何 assistant 回复，历史里也看不到 B；
3. 再次点击 `↑` 才正常发出。

`./dev.sh` 重建后仍可复现；本地已按修复后行为验证通过。

## 证据

运行时采集自 `~/.peri/threads/threads.db` 与 `.tmp/agent-tui.2026-11-02`：

1. `08:24:19.002Z` 写入一行 `thread_mq`（`source=user_input`，草稿「继续」），
   随后 5 分钟每 5 秒的轮询都读到 `rows_returned=1`，该行始终未被消费；
   `messages` 中对应 `request_id` 计数为 0。
2. `08:24:22.895Z` 出现 `send frame=Reset {... error_code: CANCEL }` 与
   `result_suspended ... reason="cancelled"`，同一毫秒 `session/input/dispatch`
   已 `response_sent=true`：RPC 正常返回，B 确实进入了邮箱。
3. 修复前日志没有 `continuation: scheduling`；修复后（`09:18:17Z`）出现
   `reason="steer"` 的 debug 记录，但仍无新 `turn_start`，直到 `09:20:29Z`
   第二次点击才 `mq_steering=true` 消费成功。

## 根因

两处缺陷叠加，缺一不可。

### A. steer 取消被当成 Stop，activation 永久抑制

`UserInputMailbox::dispatch` 对 steer 输入设置 `active.reason = Steer` 后取消
token；`peri-acp/src/host/prompt.rs` 的取消分支对**所有**取消调用
`activation.suppress()`。`activation.allow()` 只在 turn 启动时调用，之后没有
恢复点，于是 tail 调度器与 `mq_allowed` 兜底都被挡住。

### B. 预留 guard 在正常收尾后重复结算，撤回发送选择

`peri-acp/src/host/user_input.rs::schedule_mailbox` 的外层 `InputAttemptGuard`
在 `dispatch_prompt_turn_with_input` 正常返回后 Drop 仍调用 `fail_reserved`：
强制 `paused = true`、清空 `ready`、`Dispatching → Queued`，并
`withdraw_send_inputs`。该行随即不满足 `is_eligible_for_receive`，
`has_ensure_processing` 为 false，调度器静默 `continue`；
`finish_prompt_turn` 与 `withdraw_send_inputs` 都不调用 `notify_wake()`，
所以没有第二次机会。

第二次点击之所以有效：`dispatch` 重新写入 `send_selection` 并 `notify_wake()`。

## 修复

1. `prompt.rs`：取消分支中 `is_steer_interruption` 为真时跳过
   `activation.suppress()`，只记 debug；Stop 与 `suspend_scope` 语义不变。
2. `user_input.rs`：外层 guard 改名并显式结算——`Err` 分支清预留、
   `disarm_if_settled()` 为真才 `reserve_run()`；attach 未成功
   （`Ok(Value::Null)` 且 active 仍属本 ticket）保留 Drop 兜底。
3. `user_input_mailbox.rs`：`is_steer_interruption` 提升为 `pub`，新增只读
   `is_active_ticket`。
4. 回归用例：`steer_activation_test.rs`（tail 级、真实 `schedule_mailbox` +
   `run_continuation_scheduler` 无需二次点击、attach 失败保留 Drop 兜底）、
   `stop_activation_test.rs`（Stop 仍抑制 continuation）。
5. `docs/code-index/peri-acp.md` 补 steer 例外与消费路径。

不在本次范围：数据库表结构、`fail_reserved` 的跨 ticket 回滚收窄（见残留风险）。

## 验收

1. 长流式输出中排队 B 并单击一次 `↑`：A 中断、B 离开暂存区、历史中 B 恰好
   一次、随后自动出现 B 的回复，无重复 turn。
2. 窄终端（92×40）下点击仍命中 `↑`；草稿内容不被修改。
3. 流式输出中 `Ctrl+C`：排队项保持 `Queued`，不自动续跑。
4. `scripts/check-changed.sh`、`cargo fmt --check`、`clippy -D warnings` 通过；
   `peri-acp`/`peri-agent`/`peri-acp-types` 的 lib 测试全绿。
5. 反向验证：恢复无条件 `suppress()` 或无条件 `disarm` 时新用例必须失败。

## 残留风险

1. host 级 `Err`（非普通模型/工具失败）在途时，用户已发送的下一条输入会被
   弹回 `Queued` 并撤回发送选择，需要再点一次。消息不丢失，触发概率低。
   完整修复需让 `fail_reserved` 按 ticket 收窄，或拆分「释放本 ticket 预留」
   与「host 丢失任务后的全局恢复」。
2. steer 轮次仍执行 `async_result_mailbox.suspend_scope("cancelled")`，
   期间完成的后台结果不会由本次 continuation 接管。
3. Stop 的既有窄窗口（定向 cancel 被判 superseded 时不取消也不 suppress）
   未扩大，但修复后排队中的 `mq_steering` 可能被调度，从而变得可观察。
