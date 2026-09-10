# Middleware 在 v2 中可调用无效写操作且无编译期反馈

**状态**：In Progress
**优先级**：中
**类型**：重构
**创建日期**：2026-07-25
**来源**：2026-07-24 架构审查 A4；过程文档已删除，本 issue 保留可执行问题与证据
**最后核查**：2026-09-10

## 最新情况（2026-09-10，第一组 A）

已移除 `MiddlewareState` 上 10 个没有生产 middleware 消费者的失真能力，以及
`AgentContext` 内对应的 token/context/ancestor 快照。`AgentState` 自身真实 API
保留，stage 的 token tracker 和 session context 所有权不变。hook 签名仍使用
`MiddlewareState`，分阶段能力约束留待第二组 B，不能视为整个 issue 已完成。

`messages_mut()` 与初始审查记录不同：ImageMiddleware 在生产 `before_agent`
使用它，runner 会在链结束后按 MessageId reconcile，包含 Err 路径。本组将其
迁为 `replace_message(message) -> bool`：只能替换已有可见 ID，不支持 Vec
增删或重排；未知 ID 返回 false。Image 保留原 ID，消息追加仍双写 transcript
和缓存，model before→after 的消息注入路径不变。

### 旧接口消费者清单

以下为删除前的仓库调用核查；不计 trait/impl 定义，不混入 AgentState 自身
API、Transcript API 或 Atomic::store。生产 middleware 之外的 AgentState
`set_context`（executor 结果投影）等仍真实有效，不属于本组删除面。

| 旧方法 | 生产 middleware 消费 | 测试消费 / 处置 |
| --- | --- | --- |
| `set_cwd` / `set_current_step` | 0；AgentContext 空实现 | 只有适配器/mock 定义，删除 hook 面定义 |
| `store` / `own_thread_id` | 0；AgentContext 恒 None | 只有适配器/mock 定义，删除 hook 面定义 |
| `prepend_message` | 0；仅改缓存，且不标记 reconcile | `agent_context_test::test_prepend_message_emits_warning`，删除 |
| `token_tracker` / `token_tracker_mut` | 0；实际是 stage tracker 的 clone，修改不回写 | `test_token_tracker_is_default`（2 次读取）与 `test_token_tracker_mut_is_mutable`（1 次读/写），删除 |
| `ancestor_len` | 0；AgentContext 恒 0 | 只有适配器/mock 定义，删除 |
| `get_context` / `set_context` | 0；自有 HashMap 修改不回写 | `test_get_set_context_on_owned_hashmap`（2 次读、1 次写），删除 |
| `messages_mut` | `middleware/image/mod.rs::before_agent` 唯一消费者 | 旧 cache-only append 测试替换为稳定 ID replacement、未知 ID 拒绝与 legacy 适配一致性测试 |

六个实现同步迁移：`AgentState`、`AgentContext`、Agent queue 测试的 `TestState`，
以及 middleware git_watch / tool_search / mcp 测试的三个状态适配器。

### 第一组回归

- `agent_context_test.rs`：稳定 ID 替换、未知 ID 拒绝、legacy 适配语义一致；保留双写/可见消息/cwd/step/recall/queue 测试。
- `middleware_runner_test.rs`：真实 before_agent 成功和 Err 路径均回写替换、保留 ID/flags/excluded 消息，追加与 recall 不丢失，失败仍短路后续 middleware。
- `middleware/image/mod_test.rs`：真实 Image→AgentContext→runner→transcript 路径保留 Human ID 并发布附件错误块。
- 保留 `middleware/chain_test.rs::test_state_mutation_visible_across_hooks`，验证 before_model 追加对 after_model 可见。

目标 Cargo 测试与 clippy 由主 agent 统一执行；本组不改变链序、hook 签名或工具目录契约。

## 问题描述

`MiddlewareState` 同时向所有 middleware hook 暴露消息、cwd、step、token tracker、context map、recall、thread store 和 queue 等跨领域能力。v2 的 `AgentContext` 对部分写方法只记录 warning 并 no-op，因此调用方可以通过编译和 mock 测试，却在生产 v2 路径中不产生预期修改。期望 middleware 按 hook 获得最小、真实可用的 capability context，无效操作在编译期不可见，而不是运行时静默失效。

## 初始审查记录（2026-07，当前事实见上节）

当时架构审查观察到：

- `MiddlewareState` 暴露约 16 个跨领域能力，部分方法已标记 deprecated；
- `messages_mut()`、`prepend_message()`、`set_cwd()`、`set_current_step()` 在 `AgentContext` 中仅 warning 或 no-op；
- `token_tracker_mut()` warning 后仍返回 tracker，接口语义不一致；
- 每次 hook 调用由 `middleware_runner` 临时构造具有完整权限的 `AgentContext`；
- `before_agent`、`before_tool`、`after_agent` 等不同生命周期 hook 获得相同的宽接口；
- 任意 `HashMap<String, String>` context key 允许 middleware 间形成未声明耦合。

这会造成：

- 新 middleware 调用无效方法时无法从类型和测试及时发现；
- mock 实现可能提供生产实现并不存在的能力；
- 修改一个 trait 方法需要更新大量无关 middleware/mock；
- capability 的 await 安全性与生命周期约束无法表达。

## 期望改进方向

按能力拆分 middleware interface，例如 transcript view、message injection、queue access、turn metadata、approval、recall、thread persistence 等。每种 hook context 只组合其生命周期中真实可用的能力；v2 不支持的写方法从公开接口移除，而不是继续保留 no-op 适配。

具体 trait 名称和组合方式由实现计划决定，本 issue 不强制采用泛型、trait object 或 extension slot 的某一种实现。

## 与现有 Issue 的边界

- `spec/issues/2026-07-16-p1-1-stagecontext-split.md` 关注 `StageContext` 内部按 Session/Runtime/Compact/Async 生命周期分组。
- **本 issue 关注 middleware hook 的外部 capability 边界**：即使 `StageContext` 已完成拆分，middleware 仍不应获得过宽或失真的 `MiddlewareState`。
- 两项工作应协调字段归属和迁移顺序，但可以分别验收。

## 验收标准

- [ ] 形成生产 middleware 对 `MiddlewareState` 方法的使用清单，区分读取、写入、生命周期与 await 边界。
- [ ] v2 中仅 warning/no-op 的方法从生产 hook context 移除，或迁移为具有真实语义的明确 capability。
- [ ] `before_agent`、`before_tool`、`after_tool`、`after_agent` 至少使用按需能力不同的 context/interface。
- [ ] middleware 无法调用当前 hook 不支持的写操作；错误使用在编译期失败。
- [ ] 任意字符串 context key 被 typed extension、newtype key 或集中声明的命名键替代，禁止新增裸字符串隐式协议。
- [ ] 所有生产 middleware 完成迁移，不保留兼容 no-op fallback。
- [ ] 测试分别覆盖 before/after agent 与 before/after tool 的正常路径和不支持能力路径。
- [ ] middleware 固定执行顺序保持不变，并由现有/新增 contract test 证明。

## 非目标

- 不在本 issue 中重新设计整个 ReAct `StageContext`。
- 不改变 middleware 的业务顺序或功能开关。
- 不顺带重构与 capability 迁移无关的 middleware 实现。

## 关联 Issue

- `spec/issues/2026-07-25-production-middleware-order-has-no-contract-test.md` —— 在接口迁移期间锁定生产链顺序。

## 涉及文件

- `peri-agent/src/middleware/state.rs` —— 当前过宽的 `MiddlewareState` trait。
- `peri-agent/src/middleware/trait.rs` —— middleware 生命周期 hooks。
- `peri-agent/src/agent/agent_context.rs` —— v2 兼容实现及 no-op 方法。
- `peri-agent/src/agent/stages/middleware_runner.rs` —— hook context 构造与调用边界。
- `peri-middlewares/src/` —— 生产 middleware 使用方与局部测试。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 根据架构审查 A4 创建，并关联 StageContext 拆分 issue |
| 2026-09-10 | Open | In Progress | agent | 第一组移除失真 API，稳定 ID replacement 替代 Vec 修改；hook 签名收窄待后续组 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
