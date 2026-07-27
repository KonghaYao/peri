# Agent Loop 五阶段 CRRAE 简化为四阶段 RCRA

**状态**：Open
**优先级**：中
**创建日期**：2026-07-27

## 问题描述

当前 Agent Loop 是五阶段 **CRRAE**（Compact → Receive → Reason → Act → End），其中 Receive 和 End 存在职责重叠——两者都在检查 MessageQueue 并决定循环走向，但分属两个阶段导致空转浪费。经架构讨论，可以合并为四阶段 **RCRA**（Receive → Compact → Reason → Act），消除冗余的 End 阶段。

## 现状

**当前 CRRAE 循环**（`peri-agent/src/agent/stages/mod.rs`）：

```
Compact → Receive → Reason → Act → End
   ↑                有 tool_calls 跳过 End  │
   └─────────────────────────────────────────┘
                End 有 Prompt/Defer → 回 Compact
```

**问题**：

1. **Receive 和 End 职责重叠**：Receive 从 MQ 排空 Prompt/Info；End 也从 MQ 取 Prompt/Defer。两者本质上都在做"检查队列→决定走向"，但分属两个阶段导致逻辑分散。

2. **End→Compact→Receive 空转链路**：当 End 发现 MQ 有消息时，循环回到 Compact，而 Compact 在新一轮迭代开头基本必跳（预算 < 0.75），实际执行的是 Compact(跳过) → Receive(取消息)。等于两个阶段在空转。

3. **Compact 在 Receive 之前，压缩的是旧消息集**：当前 Compact 先于 Receive 执行，压缩的 transcript 不含本轮新拉入的 MQ 消息。消息拉入后再压缩才更准确。

## 期望改进方向

**目标 RCRA 循环**：

```
Receive → Compact → Reason → Act
   ↑                            │
   └──────── 有 tool_calls ─────┘
   队列空 → 退出
```

**改动要点**：

1. **Receive 合并 End 的退出判断**：Receive 在循环入口排空 MQ——取到消息继续，取不到退出。End 的 idle_should_wait 探针逻辑移入 Receive。

2. **Compact 移到 Receive 之后**：先取消息再压缩，预算判断基于完整消息集，更准确。

3. **Act 有 tool_calls 时直接回 Receive**：跳过 End 自然延续（当前已是跳过 End 回 Compact，改为回 Receive 只是去掉 Compact 的空转）。

4. **中间件钩子调整**：End 阶段无独立钩子（End 不触发 middleware），Receive 的语义从"取消息→写入"扩展为"取消息→写入→判空退出"，不影响已有 hook 契约。

## 涉及文件

- `peri-agent/src/agent/stages/mod.rs` —— 循环控制流 + StageContext 定义
- `peri-agent/src/agent/stages/receive.rs` —— 扩展退出判断逻辑
- `peri-agent/src/agent/stages/end.rs` —— 删除或融合进 receive
- `peri-agent/src/agent/stages/end_test.rs` —— 测试迁移
- `peri-agent/src/agent/stages/receive_test.rs` —— 新增退出判断测试

## 设计注意点

### 退出路径

RCRA 中 Receive 是正常退出（队列空 + 无 idle 等待）的**唯一出口**：

```
run_react_loop {
    loop {
        receive()?  // 队列空且无 idle → break
        compact()?
        let act_result = act(reason(compact_result)?)?;
        // 不管 Act 结果是什么（工具调用/回答），统一回 loop 顶部 Receive
    }
}
```

错误退出沿用现有 `?` 传播机制，不在 RCRA 改动范围。

## E2E 测试回归结果

### 修复前（Phase 6.6 预消费 bug，`123c1add`）

| 测试文件 | 失败信息 |
|----------|---------|
| `tests/scenarios/ask-user-question.test.ts` | `Error: Text "Ask User" not found (timeout: 60000ms)` |
| `tests/scenarios/goal-continuation.test.ts` | 等待数字 1-10 超时 |
| `tests/scenarios/hitl-approval.test.ts` | 等待 HITL 审批弹窗超时 |

通过的 8 个：`basic-question` `model-switch` `plugin` `clear-chat` `compact-command` `streaming-tool-interleave` `thread-switch` `user-bubble-scrollbar`

### 修复后（Phase 6.6 移除）

待回归。上述 3 个失败很可能同根因（agent loop 立即退出无输出）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-27 | — | Open | agent | 创建 |

## 修复记录

### 修复 #1（2026-07-27）

- **操作人**：agent
- **问题现象**：RCRA 重构后 agent loop 对任何用户输入均立即返回 0s，不执行 Reason/Act 阶段
- **根因**：Phase 6.6（在 `run_react_loop` 之外通过 `drain_for_receive()` 预消费队列）与 RCRA 的 Receive 退出判断冲突。CRRAE 时代 Receive 不做退出判断（退出在 End），预消费兼容；RCRA 中 Receive 用 `consumed_count == 0` 判断退出，预消费使队列变空 → 循环立即退出
- **修复内容**：
  - `peri-agent/src/agent/stages/mod.rs`：`run_react_loop` 内首次 Receive 后执行 `before_agent` middleware hooks（替代外部 Phase 6.7）
  - `peri-acp/src/session/executor_helpers.rs`：移除 Phase 6.6（预 drain+append）和 Phase 6.7（hooks 调用），改为由 loop 内部处理
  - `peri-acp/src/agent/workflow_agent.rs`：同上
- **验证状态**：单元测试全部通过（46 个 stages 测试 + 8 个 queue 测试 + 317 个 peri-acp 测试），clippy 零告警。E2E 测试待回归。
