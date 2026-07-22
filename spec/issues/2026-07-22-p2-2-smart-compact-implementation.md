# P2-2：Smart Compact 空策略实现

**状态**：Open
**优先级**：低
**类型**：功能缺口
**创建日期**：2026-07-22
**来源**：架构成熟度评估 — 工程规范维度

## Problem Statement

`peri-agent/src/agent/compact/smart.rs:9` 包含一个空策略分支，仅标记 TODO 但无实现：

```rust
// TODO: Smart Compact strategy
pub fn smart_compact(...) -> CompactResult {
    unimplemented!()
}
```

当前 Compact 策略仅 Micro（标记 truncated）和 Full（LLM 摘要），缺少一个中间策略——根据上下文重要性智能选择保留/丢弃，而非一刀切。

## 建议方案

1. 定义 Smart Compact 的选择标准（如保留最近 N 条关键消息 + 保留工具调用结果 + 保留错误消息）
2. 实现 `smart_compact()` 函数
3. 在 `determine_compact_strategy()` 中接入

## 涉及文件

- `peri-agent/src/agent/compact/smart.rs:9`
- `peri-agent/src/agent/compact/mod.rs` — 策略分发

## 风险

- **中**：需要设计策略参数和效果验证；可能影响 prompt cache 命中率
