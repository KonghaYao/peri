# 生产中间件顺序缺少可执行契约

**状态**：Open
**优先级**：低
**类型**：技术债
**创建日期**：2026-07-25
**来源**：`docs/architecture-review-2026-07-24.md` A6
**最后核查**：2026-08-11

## 最新情况（2026-08-11）

生产装配已随 3.0 L2 迁入 SessionFactory/assembly（`peri-agent/src/session/factory.rs`），middleware 注册点收敛；但仍无锁定生产链完整顺序（无条件 + 条件 middleware 各配置组合）的 contract test。

**状态**：Open（保持）

## 问题描述

生产中间件链在 `build_and_execute_agent_v2` 中通过一系列 `chain.add(...)` 手工排列，并混合无条件 middleware、权限模式条件、MCP/Hook/Workflow/LSP/Goal 开关和共享资源构造。现有测试证明 `MiddlewareChain` 的 before 正序、after 逆序，但没有锁定生产 composition root 在各配置组合下的完整名称和依赖顺序。期望生产链顺序成为可检查、可回归的契约，使错误插入或末端约束漂移在测试阶段失败。

## 现状

- `MiddlewareChain` 只保存 `Vec<Box<dyn Middleware>>` 并按注册顺序执行。
- 约 15 个基础 middleware 与 5 个条件 middleware 的排列依赖 builder 中的手工代码和项目文档。
- 当前不存在独立的 `with_system_prompt()` middleware；实际末端约束是：完整装配 chain 后，按 chain 顺序执行 `collect_prompt_contributions()`，再构造 `BaseModelReactLLM::with_system()`。
- ToolSearch、Skills、AgentsMd、Hook 等 prompt/tool 贡献顺序改变时，基础 hook 测试仍可能全部通过。
- TUI 配置切换、stdio、workflow/SubAgent provider 重建可能形成不同条件组合，当前没有统一 fingerprint 或矩阵测试证明链一致。

## 期望改进方向

最低可行方案是为每个 middleware 提供稳定 ID，并对生产 builder 输出的完整序列做精确断言。若手工条件编排继续扩张，可进一步引入声明式 `before/after/enabled_if` spec 和拓扑校验；本 issue 不强制为了单次测试立即实现拓扑排序。

## 验收标准

- [ ] 每个生产 middleware 有稳定、唯一且可测试的 ID。
- [ ] contract test 精确断言默认配置下的完整 middleware 序列。
- [ ] 测试覆盖 approve/skip 等权限模式，以及 MCP、Hook、Workflow、LSP、Goal 的启用/禁用组合。
- [ ] 明确断言完整 chain 装配结束后，先按 chain 顺序收集 prompt contributions，再构造 `BaseModelReactLLM::with_system()`；不得在链尚未完整时提前冻结 system prompt。
- [ ] 任意 middleware 被重排、遗漏、重复注册或插入错误位置时，至少一条 contract test 失败。
- [ ] composition root 可输出稳定 chain fingerprint，便于比较 TUI、stdio、workflow/SubAgent 的实际组合；日志不包含 secrets 或 prompt 内容。
- [ ] 本 issue 完成时不改变现有生产顺序，仅把既有契约显式化。
- [ ] 若采用声明式依赖，重复 ID、未知依赖和依赖环在启动或构建测试中返回结构化错误。

## 非目标

- 不在本 issue 中重新排序或删除 middleware。
- 不改 middleware hook 的业务逻辑。
- 不强制立刻以拓扑排序替代当前手工顺序。

## 关联 Issue

- `spec/issues/2026-07-25-middleware-capabilities-can-silently-no-op.md` —— capability interface 迁移期间依靠本 issue 防止链顺序回归。
- `spec/issues/2026-07-16-architecture-upgrade-checklist.md` —— 既有架构升级与链顺序约束背景。

## 涉及文件

- `peri-acp/src/agent/builder.rs` —— 生产 composition root 与中间件注册顺序。
- `peri-agent/src/middleware/chain.rs` —— 链存储和 before/after 执行语义。
- `peri-agent/src/middleware/chain_test.rs` —— 当前基础顺序测试。
- `peri-acp/src/agent/` 下相关测试 —— 新增生产链配置矩阵 contract test。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-25 | — | Open | agent | 根据架构审查 A6 创建 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
