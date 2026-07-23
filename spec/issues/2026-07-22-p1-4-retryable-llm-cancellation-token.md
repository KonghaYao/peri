# P1-4：RetryableLLM 重试不检查 CancellationToken

**状态**：Open
**优先级**：中
**类型**：Bug
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 错误处理维度

## Problem Statement

`peri-agent/src/llm/retry.rs:91-146` 中 `RetryableLLM::call()` 的重试循环不检查 `CancellationToken`。当用户取消操作（如按 Esc 中断 agent）时，正在重试的 LLM 调用不会感知取消信号，继续占用资源直到 5 次重试耗尽或成功。

症状：
- 用户取消后，后台 LLM 请求仍在进行（资源浪费 + API 费用）
- 取消后重试成功的结果也会被丢弃（因为 agent 已结束）

## 建议方案

在重试循环中每次迭代前检查 `cancellation_token.is_cancelled()`：

```rust
for attempt in 0..max_retries {
    if cancellation_token.is_cancelled() {
        return Err(AgentError::Interrupted);
    }
    // 现有重试逻辑...
}
```

CancellationToken 需从 `StageContext` 或调用方传入。

## 涉及文件

- `peri-agent/src/llm/retry.rs:91-146` — 重试循环

## 风险

- **低**：纯增量检查，不影响现有重试逻辑
