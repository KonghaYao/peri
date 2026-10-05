# [设计提案] 异步任务归属、投递与可见性

> 后续裁决：下文为旧方案背景；涉及 root scope 归属、root fallback、投递即处理及子会话生命周期的目标已由 [RCRA 消息权威](../../docs/design/rcra-message-activation.md) 替代，统一实施入口为 [新 active issue](2026-10-05-rcra-message-activation.md)。不能按下文旧 fallback 继续实施。

**状态**：已裁决（advisor-consultation → controller 决定：方案 A 采纳、D1–D4 按推荐选项采纳、H1 验证通过，见 §10）。已写回 [docs/design/session-async-tasks.md](../../docs/design/session-async-tasks.md)（该文档是异步任务控制面的权威设计；本提案补充其未定义的"子会话发起任务"归属，并修正实现的维度合并）；本提案保留为实施跟踪入口，实施待启动。
**关联**：[bg 回执信息投递缺陷批次](2026-10-04-mcp-task-receipt-delivery-defects.md)——本提案是该批次 1、4.x 的架构解法；批次 2、3 为独立健壮性/收敛修复，不在本提案范围。
**创建日期**：2026-10-04

## 1. 诊断

现状实现把两个本应正交的维度合并成了一个字段：

- **执行范围（scope）**：任务的恢复对账、关闭级联与 MCP scope 覆盖的会话范围——合理取值是**执行树根**（root）；
- **提醒归属（delivery target）**：终态结果承诺送达的会话——合理取值是**发起会话**（subagent 的 child session）。

现状 `mcp_task_owner_session_id` 同时承担两者（subagent 场景 = root），订阅与投递都按它注册（`tool_bridge.rs:291-295`、`subscription_tasks.rs:590-597,751-764`），于是发起者结构上不可达，而回执仍向发起者承诺投递。可见性（TUI 面板）也没有独立建模，被"顺带"绑到同一个 owner 上——三者叠在一个字段里，任取一个值都必然牺牲另一个语义。

对照权威设计 `session-async-tasks.md`：

- §1/§3：Session Task Manager 的终态提醒放入**本会话** MessageQueue、原子落入**本会话 canonical transcript**（稳定投递 ID 去重、可重投）——投递目标语义是"任务所属会话"；
- §3：展示事件与 Agent 消息投递**分离**（一方失败不得掩盖另一方）——设计已给出"可见性与投递分离"的先例；
- §4：scope 仅用于**发现与恢复对账**——scope 从来不是"投递承诺"的载体。

结论：设计未定义"子会话发起的外部任务"归属（空白）；实现以单一 owner 合并维度填补了空白，产生了本批次缺陷。

## 2. 目标模型（一个任务，三类关系）

| 维度 | 取值 | 用途 | 说明 |
| --- | --- | --- | --- |
| 提醒归属 delivery target | **发起会话**（直接发起本次工具调用的 session，如 subagent child） | 终态提醒的投递与唤醒 | 写入其 canonical transcript（持久）+ MQ（活跃时）；resume/load 后可见 |
| 执行范围 execution scope | **执行树根**（root） | MCP task scope、恢复对账、关闭级联、取消授权 | 保持现状语义，避免恢复范围碎化到 per-child |
| 可见性 visibility | **祖先链聚合，至少 root** | TUI 面板 / ACP 快照 | 只读投影；条目携带 initiator 身份（thread/call id/kind）；展示失败不影响投递 |

任务身份扩展：在设计的 `session_id + owner identity + owner task_id` 基础上显式增加 **initiator 归属**（发起会话 id + 调用上下文），并随 scope 发现（§4 响应）返回——使跨进程重建时投递目标可恢复。

## 3. 不变式

- **I1 回执可兑现**：发给发起者的回执所承诺的投递，必须在该会话可达（最终写入其 canonical transcript）；不可达的投递不得承诺。
- **I2 持久投递**：提醒以稳定投递 ID 原子写入归属会话 transcript（沿用设计 §3 机制）；MQ 入队不算送达；崩溃可重投、提交后拒绝重投。
- **I3 可归属**：完成通知与聚合条目携带 initiator 身份；root 无需猜测来源。
- **I4 展示与投递分离**：沿用设计 §3——展示事件失败不丢结果，投递失败不被展示掩盖。
- **I5 取消授权**：initiator 可取消自己的任务；scope owner（root）可取消整树；关闭按 scope 级联（设计 §5 不变）。
- **I6 收敛**：发起会话结束前对本会话未结算任务执行有界收敛（等待/交接）；交接信息写入其结果与通知。

## 4. 生命周期

- 发起者活跃：提醒 → 归属会话 MQ Defer → Receive 唤醒（路径不变，目标会话修正）。
- 发起者不活跃/已结束：提醒持久化于归属会话 transcript；resume/load 后按稳定投递 ID 去重可见。
- 发起者永久不恢复：scope owner 的关闭/对账流程负责结算与报告（丢失可观测，不静默）。
- 跨进程：scope owner 重建时按 §4 发现任务并恢复到 root 视图；initiator 归属从任务记录/身份字段恢复；无法恢复时降级为 root 投递并在通知中说明（边界显式）。

## 5. 各发起者类型映射

| 发起者 | 归属（投递目标） | scope | 备注 |
| --- | --- | --- | --- |
| root（ACP 会话） | root 自身 | root | 现状不变 |
| subagent | child session | 执行树 root | 本提案核心修复对象 |
| 嵌套 subagent | 直接发起层 child | 最顶层 root | 中间层同样可达（与现状断裂相反） |
| workflow 内 agent | 其执行会话（待实验确认承载） | 执行树 root | 与缺陷批次 4.2 实验联动 |
| print | print session | print session | 与缺陷批次 4.3 联动 |

## 6. 对 session-async-tasks.md 的修订点（批准后执行）

- §1：任务身份补充 initiator 归属；明确 scope 与投递目标正交。
- §3："本会话"明确为**归属会话**（发起会话）；投递不因 scope owner 不同而改道。
- §4：scope 发现响应携带 initiator 归属，支持冷恢复重建投递目标。
- §5：发起会话关闭/结束的收敛检查（I6）。
- §6：验收追加——subagent 内后台任务发起者可达；聚合视图可归属；跨进程重建后投递目标正确。

## 7. 实现影响草图（不含批次计划）

- 注册：submit 记录 initiator 与 scope owner 两个维度（现为单字段覆盖）。
- 投递：按 initiator 路由；canonical 提交复用现有 Store 原语 `append_reminder_if_absent`（H1 已验证；调用方需持有目标 root 的 Store 句柄），活跃 child 的唤醒通道由 Agent runtime 提供（不依赖 ACP SessionManager 注册——当前 child 不经过 ACP host）。
- 展示：ACP 快照/增量事件增加 initiator 字段；root 聚合投影。
- 回执：文案按投递语义生成（承诺=可兑现），测试锁定。
- 文档/提示语/e2e 一致性整改并入缺陷批次 5。

## 8. 裁决点（已裁决，理由与约束见 §10）

> D1–D4 全部按推荐选项采纳。

| # | 裁决点 | 选项（推荐加粗） |
| --- | --- | --- |
| D1 | 提醒归属模型 | **归属=发起会话**；或维持 owner 投递 + 诚实回执 |
| D2 | 可见性范围 | **祖先链聚合（root 面板保留，含 initiator 身份）**；或仅归属会话 |
| D3 | 收敛策略 | **发起者结束前有界等待 + 超时交接**；或不等待、直接交接 |
| D4 | 跨进程 initiator 恢复 | **任务记录扩展 initiator，随 scope 发现返回**；或降级 root 投递并文档标注 |

## 9. 与缺陷批次的关系

- 覆盖批次 1（1.1–1.5、1.8）与批次 4.x 的架构根因；
- 批次 2（结算静默丢弃）、3.1（scope.uncertain）为独立修复，与本提案并行、不互斥；
- 批次 5（文案/文档/测试一致性）随本提案实施同步。

## 10. 裁决记录（2026-10-04）

**途径**：advisor-consultation（Opus 顾问独立评审，意见非绑定）→ controller 逐项决定。顾问定性：方案 A 属"补充已批准设计 + 修正实现"，非推翻；裁决以 H1 为条件。

### 10.1 Controller 决定

| 顾问意见 | 决定 | 一句理由 |
| --- | --- | --- |
| 方案 A（三维分离）为目标架构 | **采纳** | 单字段合并三维已有代码证据支撑，且与已批准设计 §1/§3/§4 兼容 |
| 否决 B（维持 owner 投递 + 诚实回执）为最终架构 | **采纳** | B 只治文案、发起者仍不可达；回执真实性作为过渡止血并入缺陷批次 5 |
| 否决 C（child 目录整体迁移） | **采纳** | 复杂度与不可逆性高，收益不匹配 |
| D1 提醒归属=直接发起会话 | **采纳** | 回执承诺必须可兑现（I1），与设计 §3"本会话"语义一致 |
| D2 可见性=祖先链聚合 | **采纳（带约束）** | 只读投影：不夺回投递权、不产生第二结果权威源、root 视图不生成新终态；条目以稳定任务身份 + terminal transition ID 去重 |
| D3 发起者结束前有界等待 + 超时交接 | **采纳** | 等待必须有界、交接必须可观测（I6）；具体界限与交接内容在写回时定义 |
| D4 任务记录扩展 initiator、随 scope 发现返回 | **采纳** | 跨进程重建投递目标；root 降级保留为显式可观测兜底 |
| 风险：§5 关闭级联 | **采纳为约束** | child 关闭不得取消 root scope 任务；root 关闭仍按 scope 级联 |
| 风险：initiator 安全边界 | **采纳为约束** | initiator 取自可信 session binding，不接受模型参数/字符串自称 |
| H1 最低验证（非活跃 child 的 canonical 写入） | **已验证（通过）** | 见 10.2 |

### 10.2 H1 验证证据（HEAD `3a999a1e`，2026-10-04）

- 原语已存在且为 thread 寻址：`SessionResources::append_reminder_if_absent(thread_id, message_id, reminder)`（`peri-acp-types/src/session_resources.rs:827`），不要求目标会话活跃或经 ACP 注册。
- 原子 + 幂等：SQLite `append_terminal_reminder`（`peri-resources/src/sessions/sqlite_store/session_data/async_task.rs:6`）单 `BEGIN IMMEDIATE` 事务；同 ID 同内容重放返回 `Ok(false)` 不重复写入；同 ID 异内容报错。
- 写入前提：事务内 `assert_owner`（`.../session_data/owner.rs:411`）要求目标线程所属 root 的 execution owner 有效（epoch+nonce，未过期未释放）——与所有会话写入同一约束，不要求 child 活跃。
- 结论：**durable handoff 无需新的持久化位置**——child 的 canonical transcript 即持久化位置；剩余工作为投递路径的调用点（现 `deliver_task_reminder` 仅做 MQ push，canonical 提交发生在目标会话自身 Receive 的 `already_present` 分支）与活跃 child 的唤醒路由。
- 边界：进程退出后由新 owner 续投（幂等）；root lease 释放后的写入失败属预期（会话已关闭）。
- 实现期待验证项（H1 未覆盖）：投递路径持有 root Store 句柄的接线方式、活跃 child 的唤醒注册，写回后由实现与测试验证。
