# ViewModelsSnapshot 大重构：单层持久化向量 + 消灭所有旁路

**状态**：Open
**优先级**：中
**创建日期**：2026-07-08

## 架构审查记录

三路 `explore` subagent 并行审查，2026-07-08：

| 审查维度 | 结论 | 关键发现 |
|---------|------|----------|
| 追加性能 | PASS | im::Vector O(log n) 比当前 per-chunk O(n) clone 量级更优；写锁持有时间降低；结构共享内存友好 1000×；content_hash diff 覆盖区间从 TurnDone-only 扩展为全 chunk |
| 数据一致性 | CONCERN | UserBubble 在提交→PromptStarted 之间有空白窗口；loading 中提交回显缺失；TurnDone 仍需要归档工作（提案"不搬运"描述不准） |
| 渲染语义 | CONCERN | tool_card_ids 确认死字段（零读取）；SubAgent 不改增量追加则 im::Vector 收益有限；1s poll 需改为 generation 检测 |

### 审查引发的设计决策

| 决策点 | 选项 | 结论 |
|--------|------|------|
| UserBubble 可见性窗口 | A channel 中转 / B 同步接口 / C 保留旁路 | **A**——submit_consumer 收到文本后通过 AcpEventData 通道转发给 bridge，bridge 立即追加 + push。零延迟，唯一写入路径 |
| loading 中提交回显 | D channel 中转 / E 渲染虚影 | **D**——复用 A 的同一条通道，`LocalUserBubble` 事件统一处理 loading/no-loading 两条路径 |
| im::Vector 层级策略 | H 分级 / F 全级 / G 暂不换 | **F**——顶层 items + SubAgent children 都换 `im::Vector`，且都改增量 `push_back`（非 clear+rebuild） |

## 问题描述

`ViewModelsSnapshot` 当前设计有三个结构性问题：(1) `committed`/`current_turn` 分裂把 Agent 层的回合概念漏到 UI 数据结构里，渲染层被迫拼接两个 Arc；(2) `append_local_user_bubble` 绕开 BridgeState 直接写 atom + 自己同步 RENDER_CACHE，形成第二条渲染路径；(3) `Arc<[TuiRenderUnit]>` 不支持高效追加——每次 push 都要 O(n) clone 全量。

目标是**把整个 TUI 数据流退化到一层**：单一 `im::Vector<TuiRenderUnit>` 作为全部消息的容器，所有追加走唯一路径，TermDone 不搬运数据。

## 现状

```
┌─ 写入者 ──────────────────────────────────────────────────────────┐
│                                                                   │
│  push_view_models(BridgeState)  ← bridge 每次事件写 atom           │
│  append_local_user_bubble()     ← input_area 绕开 bridge 写 atom   │
│  sync_render_cache()            ← 同上，自己同步 RENDER_CACHE      │
│  push_view_models_for_reset()   ← /clear / thread 切换             │
│  submit_consumer, thread_load   ← /clear, 再写一遍空 snapshot      │
│                                                                   │
│  ┌─ 问题 1: 多个写入者互相覆盖                                    │
│  └─ 问题 2: sync_render_cache 重复 render_bridge 的逻辑           │
└───────────────────────────────────────────────────────────────────┘

┌─ 数据结构 ────────────────────────────────────────────────────────┐
│                                                                   │
│  ViewModelsSnapshot {                                             │
│      committed:    Arc<[TuiRenderUnit]>  ← 已归档（冻结线）        │
│      current_turn: Arc<[TuiRenderUnit]>  ← 流式（会变）           │
│  }                                                                │
│                                                                   │
│  ┌─ 问题 3: 冻结是 Agent 层概念，UI 不需要                         │
│  └─ 问题 4: 渲染必须拼 committed + current_turn                   │
└───────────────────────────────────────────────────────────────────┘

┌─ 父子关系 ────────────────────────────────────────────────────────┐
│                                                                   │
│  TuiSubAgentGroup.view_models        ← 容器嵌套                   │
│  TuiAssistantBubble.tool_card_ids    ← 死字段，无人使用            │
│                                                                   │
│  ┌─ 问题 5: tool_card_ids 是死字段                                 │
│  └─ 问题 6: 顺序即关系，AB-TC 前后关系已经够用                     │
└───────────────────────────────────────────────────────────────────┘
```

## 期望改进方向

1. **单层列表**：`items: im::Vector<TuiRenderUnit>`，去掉 committed/current_turn 概念
2. **唯一写入路径**：`push_view_models` 是唯一写 VIEW_MODELS atom 的函数，所有追加必须经过它
3. **O(1) clone**：`im::Vector` clone 只 bump 引用计数，追加 O(log n)
4. **保留 SubAgent 嵌套**：`TuiSubAgentGroup` 保留为叶子容器（children 也是 `im::Vector<TuiRenderUnit>`），不拍平
5. **删除死字段**：移除 `TuiAssistantBubble.tool_card_ids`
6. **删除 sync_render_cache**：render_bridge 独享 RENDER_CACHE 写入权

## 涉及文件

- `peri-tui/src/kit/atoms.rs`（第 52-61 行）—— `ViewModelsSnapshot` 定义
- `peri-tui/src/kit/acp_events.rs` —— `push_view_models`、`push_view_models_for_reset`、`push_acp_state`
- `peri-tui/src/kit/acp_types.rs` —— `build_view_models`、`TuiToolCard`、`TuiAssistantBubble`
- `peri-tui/src/kit/input_area.rs`（第 656-715 行）—— `append_local_user_bubble`、`sync_render_cache`
- `peri-tui/src/kit/render_bridge.rs` —— `rebuild_entries`、`rebuild_current_turn`、`extract_hashes`
- `peri-tui/src/kit/tui_render_unit.rs` —— `TuiAssistantBubble`（删除 `tool_card_ids` 字段）、所有 `TuiRenderUnit` 变体
- `peri-tui/src/kit/submit_consumer.rs` —— `/clear` 时写入空 snapshot
- `peri-tui/src/kit/thread_load_consumer.rs` —— thread 切换时写入空 snapshot
- `peri-tui/src/kit/panels/agent.rs`、`status.rs`、`tasks.rs` —— 从 committed/current_turn 改为从 items 派生统计

## 设计要点

### 数据结构

```
ViewModelsSnapshot {
    items: im::Vector<TuiRenderUnit>,  ← 全部消息的单层容器
    generation: u64,                   ← 递增版本号，render_bridge 据此判断是否重读
}

TuiSubAgentGroup {
    children: im::Vector<TuiRenderUnit>, ← 同样换 im::Vector（增量 push_back）
    // 不再全量 clear+rebuild——每次 append_text/start_tool/end_tool 时 children.push_back()
}
```

### UserBubble 通道（替代 append_local_user_bubble 旁路）

```
SubmitRequest::AgentText(text)
  ├─ is_loading=false → submit_consumer 发: ① ACP prompt + ② AcpEventData::LocalUserBubble(text)
  ├─ is_loading=true  → push INPUT_BUFFER + submit_consumer 只发: AcpEventData::LocalUserBubble(text)
  └─ bridge 收到 LocalUserBubble → items.push_back(UserBubble(text)) → push_view_models
```

`AcpEventData` 新增变体 `LocalUserBubble(String)`。`AcpEventWithEpoch` 通道承载，bridge 统一消费。零新 mpsc channel。

### 操作成本

```
流式追加一条 VM      → O(log n) push_back + O(1) clone items
TurnDone 归档        → 状态变更：is_running=true → false（VM 已在 items 中）
                      → INPUT_BUFFER 内容通过 LocalUserBubble 通道已在 items 中，TurnDone 不重复追加
/clear 清空           → Vector::new()
render_bridge 读取    → 遍历 items（无 committed/current_turn 拼接）
1s poll Bash timer    → 比较 generation 而非 Arc::as_ptr（current_turn 已不存在）
```

### 删除项

- `committed` / `current_turn` 两个 Arc 字段
- `append_local_user_bubble` 函数
- `sync_render_cache` 函数
- `TuiAssistantBubble.tool_card_ids` 字段（零读取的死字段）
- `rebuild_current_turn` 函数
- `has_turn_done` 字段（单层列表下不需要 fallback guard）
- `VmKey::Committed` / `VmKey::CurrentTurn` 枚举变体——退化为 `VmKey::Item(usize)`

### 保留项

- `CurrentTurn` 结构体——仍作为流式消息的临时积累器（text / reasoning / tool_cards / subagents 在此积累，build_view_models 产出 VM 后由 push_view_models 追加到 items）
- `BridgeState.current_turn`——不变
- `SessionPhase` / `session_state`——不变
- `BRIDGE_RESET_COUNTER`——不变

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
