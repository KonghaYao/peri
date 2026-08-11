# Middleware 在 v2 中可调用无效写操作且无编译期反馈

**状态**：Open
**优先级**：中
**类型**：重构
**创建日期**：2026-07-25
**来源**：`docs/architecture-review-2026-07-24.md` A4
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

宽 middleware trait/no-op 能力仍在：`agent_context.rs:119/155` 对 `set_cwd()`/`set_current_step()` 仍为 no-op（v2 中由 TurnContext 管理），调用方可编译通过、mock 测试通过，但生产 v2 路径无效果，无编译期反馈。

**状态**：Open（保持）

## 问题描述

`MiddlewareState` 同时向所有 middleware hook 暴露消息、cwd、step、token tracker、context map、recall、thread store 和 queue 等跨领域能力。v2 的 `AgentContext` 对部分写方法只记录 warning 并 no-op，因此调用方可以通过编译和 mock 测试，却在生产 v2 路径中不产生预期修改。期望 middleware 按 hook 获得最小、真实可用的 capability context，无效操作在编译期不可见，而不是运行时静默失效。

## 现状

架构审查观察到：

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

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
