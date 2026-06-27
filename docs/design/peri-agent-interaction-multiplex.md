# peri-agent Interaction Multiplex 设计

> 简要说明 | 日期：2026-06-24

## 1. 用途

MultiplexBroker 是 HITL 审批系统的多路复用器。当系统同时连接多个审批通道时（TUI 终端 + 微信/Slack/飞书等 Channel），MultiplexBroker 将审批请求广播到所有通道，取最先响应的结果。

```
审批请求
  ├─ TUI Broker（本地终端）
  ├─ Channel Broker（微信）
  └─ Channel Broker（Slack）
       ↓ 竞速
  最先响应者胜出 → Approve/Reject
```

## 2. 设计

- **竞速机制**：所有 broker 通过 `tokio::spawn` 并行执行，首个响应通过 mpsc channel 返回
- **孤儿任务容忍**：首个响应后其余 spawned task 继续后台运行直到自然结束（Channel Broker 有 5 分钟超时），不会无限泄漏
- **来源标记**：返回的 `ApprovalDecision` 标记 `source` 字段，外部可区分响应来源
- **单 broker 优化**：仅 1 个 broker 时跳过 spawn + mpsc，直接调用

## 3. 约束

- 不支持 `AskUserQuestion` 交互类型——不适合竞速
- 待演进：当前无 CancellationToken 提前取消未使用的 broker
