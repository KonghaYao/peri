# P1：最终准入不重查 Stop/代际，已停止的轮次仍发起模型请求

**状态**：已修复，待 PR/CI 验收。
**优先级**：P1。
**类型**：错误（核心功能 / 执行准入）。
**创建日期**：2026-11-10。
**来源**：4.7.1 复核证据包（`review-probes.patch`）+ 探针复现。
**确认方式**：cargo 探针在基线 `4ba78d7e` 实际复现；主 agent 复核代码路径。

## 确认证据

以下行号均指修复前基线 `4ba78d7e`；修复后的位置按函数/块名描述。

- 探针 `review_stop_after_eligibility_queries_blocks_model_execution`
  （`peri-acp`，`host::requests::tests::activation_tests::admission_activation_tests`）
  失败：`left: 1, right: 0`，断言信息
  `Stop acknowledged before admission must prevent model I/O`。
  位置：`peri-acp/src/host/admission_activation_test.rs:232`。
- 探针 `review_new_prompt_after_eligibility_queries_supersedes_continuation`
  失败：`left: 2, right: 1`，断言信息
  `new input must supersede the old turn`。位置：`:281`。
- 最终准入块只检查关闭事实：
  `peri-acp/src/host/prompt_dispatch.rs:422-435`
  （`closing` / `close_in_flight > 0` 之外无任何取消或代际复查）。
- 用户 Stop 会递增代际：`prompt_dispatch.rs:133`
  （`state.continuation_epoch += 1`）；原有门闩
  `continuation_still_valid`（`peri-acp/src/host/continuation.rs:125-127`）
  只在前置查询分支使用，最终准入没有调用。
- 用户 prompt 的取消令牌在 `prompt_dispatch.rs:211-216` 发布；最终准入
  不检查 `state.cancel_token.is_cancelled()`，因此发布后、开跑前的 Stop
  不会阻止模型 I/O。

## 根因链

1. 入口完成一次资格查询并发布取消令牌（观察：`prompt_dispatch.rs:211-216`）。
2. 测试门闩 `admission_gate`（`cfg(test)`，`:442`）在最终准入前暂停；此时
   Stop 或新 prompt 把代际前推并取消令牌（观察：`:133`）。
3. 最终准入块（`:422-435`）只复核 `closing`/`close_in_flight`，未复查
   `continuation_epoch` 与 `cancel_token`（观察）。
4. `continuation_in_flight = true` 后继续 durable 提交并调用模型（观察：
   `:446` 起）。
5. 用户可见后果：Stop 后仍有 1 次模型请求；新输入后旧轮次未被取代，
   合计 2 次请求（观察 + 推断：探针计数）。

## 影响面

- 用户影响：Stop 不能立即止损；旧轮次与新输入并行产生不必要的模型调用，
  可能写入与用户意图不符的结果。
- 受影响模块：`peri-acp` host 的 prompt 准入与续跑调度。
- 可规避性：无稳定规避；只能重复 Stop 或等待请求结束。

## 修复方案

- 在 `prompt_dispatch.rs` 的最终准入块内补一次权威复查：
  - continuation（`continuation_epoch.is_some()`）：复用
    `continuation::continuation_still_valid(state, epoch)`；
  - 所有路径：`state.cancel_token` 若存在且已取消，视为不可准入。
- 拒绝时沿用现有 `!dispatchable` 分支的语义：清理
  `continuation_in_flight`/取消令牌、`mark_recovery_launch_failed`（如适用），
  返回 `Ok(Null)`，不得进入 durable 提交。
- 明确不做：
  - 不改变 epoch 递增时机与 take 语义；
  - 不改 listener 重查预算；
  - 不新增准入阶段或额外存储查询；
  - 不做 schema 变更。
- 回归测试（把探针转为正向断言）：
  - Stop 在资格查询后、最终准入前到达 → 模型请求数为 0；
  - 新用户输入在同样窗口到达 → 旧续跑中止，模型请求数为 1；
  - 正常 continuation 不受影响。

## 验证记录

- 回归测试（`peri-acp`）：`stop_after_eligibility_queries_blocks_model_execution`、
  `new_prompt_after_eligibility_queries_supersedes_continuation`、
  `continuation_after_eligibility_queries_still_dispatches` 均通过。
- `peri-acp --lib` 全量：1242 passed，0 failed，2 ignored。
- `./scripts/check-changed.sh` 与 `cargo fmt --check` 通过。
- 主 agent 复核：3 项回归测试实跑通过。
- 用户 prompt 的 close 语义保持 `-32010`；Stop 返回 `Ok(Null)` 并回滚准入。
