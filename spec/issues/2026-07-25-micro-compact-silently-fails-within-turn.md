# Micro Compact 同 turn 内静默失效

> 状态：Fixed | 优先级：P0 | 日期：2026-07-25

## 现象

Micro Compact 标记了 truncated 之后，同 turn 的 Reason 阶段 LLM 仍看到完整未压缩的原始消息。

## 根因

`plan_micro()` 在 Compact 阶段和 Reason 阶段都被调用，但函数内部跳过了已有 `truncated` 标记的消息。

```
Compact 阶段调用 plan_micro()（dry-run，决定要不要 compact）
  → 消息还没有 truncated → plan 包含压缩动作 → ok
    
micro_compact() 应用 truncated 标记

Reason 阶段再次调用 plan_micro()（为 render_llm_view 生成投影计划）
  → 消息已经有 truncated → 被跳过 → plan 为空
  → plan.has_changes() == false
  → fallback 到 visible（完整未压缩消息）
  → LLM 看到完整内容
```

**关键代码**：`peri-agent/src/agent/compact_v2/planner.rs:245`

```rust
// planner.rs:245 附近
// 跳过已有 truncated flag 的消息（避免重复）
if transcript.flags(msg_id).truncated {
    continue;
}
```

**fallback 路径**：`peri-agent/src/agent/stages/reason.rs:79-107`

```rust
// reason.rs:79-107
let plan = plan_micro(&guard, config);  // ← 已有 truncated → 空 plan
if plan.has_changes() {
    let view = render_llm_view(&guard, &plan, &caps)?;
    messages_snapshot = view;
} else {
    messages_snapshot = guard.visible_messages();  // ← 完整消息，未压缩
}
```

## 影响范围

- **Micro Compact**：同 turn 内**完全失效**——Compact 阶段打了标记，但 Reason 阶段 LLM 看不到压缩效果。跨 turn 不受影响（下一轮 compact 前消息已经有 truncated 标记，新一轮 plan 可能选中其他消息）。
- **Smart Compact**：同样走 `plan_micro` → 同样受影响。
- **旧版 truncated_content(100)**：旧版在 Reason 阶段独立循环每条消息，不依赖 plan。新版 `render_llm_view` 依赖 plan 是否存在，但 plan 不存在时 fallback 直接返回原始消息，而不是像旧版那样逐条截断。

## 修复方向

### 方案 A：plan_micro 增加参数（推荐）

给 `plan_micro()` 加 `skip_existing_truncated: bool`：

```rust
pub fn plan_micro(
    transcript: &MessageTranscript,
    config: &CompactConfig,
    skip_existing_truncated: bool,  // 新增
) -> MicroCompactPlan;
```

- Compact 阶段传 `true`：跳过已有 truncated，只生成新增的压缩计划
- Reason 阶段传 `false`：不跳过，为所有需压缩的消息生成完整的投影计划

### 方案 B：Reason 绕过 plan，直接用 truncated flag

在 Reason 阶段，如果 `plan.has_changes() == false`，不 fallback 到 `visible`，而是对所有有 `truncated` 标记的消息应用默认投影：

```rust
// 对每个有 truncated 标记的消息，生成 legacy 投影
if !plan.has_changes() {
    // 找所有 truncated 消息，生成 default action
    // 然后用 render_llm_view 应用
}
```

但这样 Reason 阶段需要知道"默认投影动作"是什么——逻辑分散。

### 方案 C：Compact 阶段缓存 plan

Compact 阶段生成 MicroCompactPlan 后缓存起来，Reason 阶段直接使用缓存的 plan，不重新调 `plan_micro()`。需要在 StageContext 或 Transcript 上存储。

## 设计反思

问题的根源是 `plan_micro()` 承担了双重职责：
1. 在 Compact 阶段作为"dry-run 规划器"（需要跳过已有 truncated）
2. 在 Reason 阶段作为"投影计划生成器"（需要包含已有 truncated）

这两个场景对"是否跳过已有 truncated"的要求是对立的。方案 A 通过参数消歧，是改动最小的修复。

## 验证标准

修复后，以下场景应通过测试：

1. **同 turn Micro → Reason**：Compact 打完 truncated → 同 turn Reason 阶段 LLM 看到的是压缩后内容（不是完整原文）
2. **跨 turn Micro → Reason**：上一 turn 打完 truncated → 下一 turn Reason 阶段 LLM 看到的仍是压缩后内容
3. **空 transcript**：无 truncated 标记时 Reason 正常走 `plan_micro` 路径
4. **compact 连续两轮**：第一轮 Micro 有效 → 第二轮 Micro 不应重复标记已有 truncated 的消息

## 修复记录

### 修复 #1（2026-07-25）

- **操作人**：agent
- **用户原意**：修复 Micro/Smart Compact 同 turn 内完全失效的 P0 bug——truncated 标记从未被任何代码路径读取用于截断
- **修复内容**：采用方案 A，给 `plan_micro()` 加 `skip_existing_truncated: bool` 参数。Compact 阶段传 `true`（跳过已有 truncated，防止重复标记），Reason 阶段传 `false`（为已有 truncated 消息生成完整投影计划）。涉及 7 个文件（planner.rs、mod.rs、micro.rs、smart.rs、reason.rs、planner_test.rs、_test.rs）
- **涉及 commit**：待提交
- **验证状态**：已验证（686 tests pass）

## 状态变更记录

| 日期 | 原状态 | 新状态 | 操作人 | 操作说明 |
|------|--------|--------|--------|----------|
| 2026-07-25 | 待修复 | Fixed | agent | 方案 A：plan_micro 加 skip_existing_truncated 参数 |
