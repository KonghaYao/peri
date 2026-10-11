# 后台结果在预算检查前被消费，失去后续处理资格

- 状态：已按用户授权集成到 `pre-release/main` 并完成集成验证，待真实场景验收；尚未核对用户现场日志。
- 基线：`f835afd325f090d64481f9218ebda4ab8610b6ab`；工作目录 `/Users/konghayao/code/ai/peri-bg-budget-fix-20261008`。
- 集成来源：修复提交 `f477fb25485ced06805821fe9247b042f5022012`；pre-release 集成基线 `7623f3d89653a31313e3cd163c117797f7949c19`。
- 范围：当前进程的 Required / EnsureProcessing 后台结果，不涉及数据库结构、冷恢复或绕过迭代预算。

## 修复前的确定性行为

`run_react_loop` 先执行 `run_receive`，领取消息并提交 Transcript，再检查语义迭代预算。
若后台完成消息在最后一次模型推理期间到达，下一次 Receive 会领取它，但随后返回
`MaxIterationsExceeded`，不进入处理该结果的 Reason，也不保留其待处理队列资格。

因此结果不是发布失败：生产完成回调已成功接纳 Required 消息，结果也已进入历史；
丢失的是尚未完成的模型处理义务。宿主基于 MQ 的续跑复查无法发现已经离开队列的结果。
使用同一 Transcript / MQ 开始新 run，也会直接结束，而不处理仅存在于历史中的该结果。

这与 `docs/design/rcra-message-activation.md` 的失败尝试不得阻止新接纳 Required 输入
激活的目标存在冲突。修复必须保留预算上限，不以无限重试或重放历史代替进程内处理资格。

## 修复与边界

loop 入口记录队列 admission watermark，预算耗尽时交给 Receive。
Receive 对当前有限批次分区：仅将入口边界后新增、模型可见的 Required /
EnsureProcessing 原始消息交还 MQ，且不提交 Transcript；既有 `push_batch` 保留
delivery ID、接纳序号及全队列顺序。`budget_deferred_count` 驱动预算错误出口，
避免剩余消息造成 Receive/idle 空转。超限日志记录保留数量和入口边界。

不提前跳过首次 Receive：首批零预算仍写历史后超限，零预算空队列正常结束。
当前 run 绑定输入、工具结果和已进入 Reason 后失败的输入不泛化回队。
child 跨 bounded idle 继续扣除累计 step，不重置预算。HookStopIntent 与结果同批时
按既有行为消费整批、停止优先；显式用户 Stop/关闭继续由宿主抑制激活。
重复 delivery 的既有唤醒行为不在本修复范围内。

稳定机制与入口已同步到 `docs/design/rcra-message-activation.md`、
`docs/code-index/peri-agent.md` 和 `docs/code-index/peri-acp.md`。

## 修复回归

测试位于 `peri-agent/src/agent/stages/loop_iteration_test.rs`：

- `background_completion_at_budget_boundary_retains_followup_eligibility`：
  预算为 1，第一次模型推理中调用生产后台完成回调。验证回调成功、消息具有
  EnsureProcessing；随后 loop 超限、只调用一次模型，终态 delivery 尚未提交历史，
  MQ 保留原 delivery ID 和接纳序号；下一 run 的模型请求包含一次结果，提交后 MQ 为空。
- `background_completion_during_reason_runs_followup_with_remaining_budget`：
  同一生产投递路径、预算为 2，第二次模型请求包含一次结果并正常结束。

补充覆盖耗尽预算后的 idle 唤醒、64 条批次边界、零预算续跑与再次模型失败不自旋、
混合策略/过期绑定、HookStopIntent 两种批次顺序，以及 child 跨 bounded idle 的累计预算。

`peri-acp/src/host/budget_activation_test.rs` 使用真实 host dispatch、HTTP Model 与
continuation scheduler，耗尽 root 默认 500 次 Reason 后自动续跑；后续请求只包含
一次结果，canonical 历史按原 delivery ID 只提交一次，接纳序号不刷新，且等待
done 通知和宿主清理全部完成后断言。辅助模型请求单独匹配，不混入 Reason 计数。

这些测试不调用外部真实模型，不覆盖 metadata 长期 Pending 等其他候选；
修复通过不等于用户现场唯一根因已经确认。

## 验证

```bash
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-agent --lib agent::stages
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-agent --lib session::subagent::child_runner
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-acp --lib host::requests::tests::activation_tests
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-acp --lib session::activation::tests
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-agent --doc
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-agent --lib agent::stages::tests::loop_iteration_tests
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-agent --lib agent::stages::receive::tests
./scripts/cargo-rmcp-patched.sh test --locked --offline -p peri-acp --lib budget_boundary_result_is_automatically_processed_by_host_scheduler
./scripts/cargo-rmcp-patched.sh clippy --locked --offline -p peri-agent -p peri-acp --lib --tests
```

最终针对性范围分别通过 **156、5、12、4、13** 项，无失败。宿主 12 项全模块
运行耗时 9.98 秒，包括真实预算边界自动续跑。
最终独立重跑宿主预算边界用例 1 项通过，测试执行耗时 7.40 秒（不含编译）。
主审独立复跑 loop_iteration_tests **15/15**、ACP activation_tests **12/12**
通过（宿主模块执行耗时 12.26 秒），格式、diff 和 8 个改动 Rust 文件规模检查全部通过；
Astra 核心 review 认可，最终复核完成。
最后独立重跑 loop 15 项和 Receive 12 项，均通过；这些是上述 156 项范围的子集，
不重复计入独立覆盖数量。Clippy 退出 0（保留未修改文件的既有警告），本次 Rust
文件 rustfmt check、`git diff --check` 均退出 0；依赖层检查 22 条规则、0 违规。

过程中的失败与重跑：基线描述性复现 10 项通过；将原缺陷测试转为修复断言后
按预期红灯 1 项。补充 ReceiveOutput 字段时首次编译因旧测试构造缺字段失败，已补齐。
宿主夹具两轮编译因跨 crate 私有 delivery helper/default 常量引用失败，现改为公开
SessionConfig 默认值和实际发布的队列身份；没有开放产品 API。首次可编译宿主测试
因夹具跨 await 保留 DashMap Ref 阻塞，被单独终止；已在复制队列后释放 guard。
随后一轮在 5.98 秒结束，因混入辅助模型请求计数 502 而非 501 失败；分离 mock 时
一轮因广匹配辅助 mock 抢先响应而失败，已改为互斥匹配。最终宿主模块全通过。

全库文件规模扫描退出 1：8 个存量测试超限，0 个源码超限；本次修改的源码/测试均
小于 1000 行。构建保留既有 dead_code 警告，未修无关代码。

## 变更路径

- `peri-agent/src/agent/stages/mod.rs`
- `peri-agent/src/agent/stages/receive.rs`
- `peri-agent/src/agent/stages/loop_iteration_test.rs`
- `peri-agent/src/agent/stages/receive_test.rs`
- `peri-agent/src/agent/stages/stages_test.rs`
- `peri-agent/src/session/subagent/child_runner_test.rs`
- `peri-acp/src/host/activation_test.rs`
- `peri-acp/src/host/budget_activation_test.rs`
- `docs/design/rcra-message-activation.md`
- `docs/code-index/peri-agent.md`
- `docs/code-index/peri-acp.md`
- `spec/issues/2026-10-08-background-result-consumed-before-budget-check.md`

## pre-release 集成验证

沿用上面的精确测试命令，在合并后的最新 pre-release 源码上重新验证：stages 156 项、
child runner 5 项、宿主 activation 12 项、session activation 4 项、Agent doc tests
15 项，合计 192 项通过，0 失败、0 ignored。宿主测试包含真实 scheduler 自动续跑。

首轮集成验证遇到跨 worktree 共享 target 的旧依赖产物，报当前源码已有的
`reminder_egress_allowed`、`output_token_limit` 和 `CompactSummaryBudgetInvalid` 缺失。
仅清理并重建 peri-model / peri-acp-types 的编译缓存后，上述验证全部通过，没有修改
无关源码；同时移除了本任务创建的共享 target 链接，修复 worktree 后续独立使用缓存。

## 下一步

1. 核对现场后台任务身份、`MessageQueueDrained` 以及迭代上限日志。
2. 完成真实场景验收；本次只合并本地 pre-release 分支，不推送远端。
3. 本记录在集成及真实场景验收关闭时按 DOC-HISTORY-001 移除，稳定机制已归位权威文档。
