# v2 重做剩余路线图

> 接手者按本文档顺序推进。每个 Stage 完成后更新 `2026-06-25-stage2-complete.md`（或新建快照）。

## 已完成 Stages

- ✅ **Stage 1**（Top 1/4/5/8）—— 详见 `2026-06-25-stage1-complete.md` §3（已合并入早期 commit）
- ✅ **Stage 2**（Top 2/3/6/7）—— 详见 `2026-06-25-stage2-complete.md`（4 commits: `e0dfda30`/`e11c79a7`/`004bdeab`/`18903fd4`）
- ✅ **Stage 3**（Top 9/10）—— 详见 `2026-06-25-stage3-complete.md`（2 commits: `531a8d82`/`35911e7b`）
- ✅ **切默认**（2026-06-25）—— 详见 `2026-06-25-v2-default.md`（1 commit：翻转 executor + CLAUDE.md + 快照）
- ✅ **Stage 4**（Langfuse Generation 端到端完整性，Top 11+12）—— 详见 `2026-06-25-stage4-complete.md`（1 commit: `9d3c1bc6`，单 commit 涵盖紧耦合的两 Top）

---

## 手动 smoke test（切默认前必跑）

详见 `verification.md` 「手动 smoke test 矩阵」段落。要点：
- 跑 `PERI_USE_V2=1 cargo run -p peri-tui -- -a`
- 对比 v1（`PERI_USE_V2` 未设置）与 v2 同 prompt 的行为
- 矩阵覆盖：对话 + 工具调用 + Compact + Cancel + SubAgent + DTO 渲染

---

## 切默认（已完成 2026-06-25）

> ✅ **已完成**。v2 现为默认，`PERI_USE_V1=1` 回退。

**目标**：翻转 `PERI_USE_V2` 默认值，v1 改为 opt-in。

### 改动点
- `peri-acp/src/session/executor.rs::run_session_loop` —— 默认走 v2，`PERI_USE_V1=1` 才回退
- `CLAUDE.md` —— 更新「v2 架构状态」段落
- 通知用户：通过 `docs/blogs/` 发版说明

### 验证
- 至少 1 周的 dogfood：开发者本地默认走 v2，监控是否有回归
- 若发现问题，临时通过 `PERI_USE_V1=1` 回退

---

## P5 — 完全消除双轨（独立里程碑，4–6 人周）

**前置条件**：Stage 2/3 完成 + v2 已切默认 + 至少 2 周 dogfood 无回归。

### P5.1 SubAgent 三件套迁移
**目标**：把 SubAgent 工具从 v1 `executor.execute()` 迁移到 v2 stages。

**文件**：
- `peri-middlewares/src/subagent/tool/define.rs` —— 定义 SubAgent
- `peri-middlewares/src/subagent/tool/execute_bg.rs` —— 后台 Fork
- `peri-middlewares/src/subagent/tool/execute_fork.rs` —— 同步 Fork
- `peri-middlewares/src/subagent/mod.rs` —— builder / 装配

**关键设计点**：
- Fork：父子 transcript 关系（ancestor），需要 v2 `MessageTranscript` 支持父子链接
- Background：独立 Session + 独立 EventBus，结果通过 `BackgroundTaskCompleted` 事件回传
- Cancel policy：父 cancel 传播到子（已有 `Session::new_with_cancel`），需验证 fork 场景

**估算**：~1200 行（含 91 测试中相关的 ~30 个迁移）

### P5.2 Hook executor 迁移
**目标**：把 hook agent 执行从 v1 `executor.execute()` 迁移到 v2 stages。

**文件**：
- `peri-middlewares/src/hooks/executor.rs`
- `peri-middlewares/src/hooks/middleware.rs`

**估算**：~300 行

### P5.3 抽取 build_agent_components
**目标**：让 v2 builder 不再构造 ReActAgent，直接构造中间件链 + LLM。

**文件**：
- `peri-acp/src/agent/builder.rs` —— 抽取公共装配函数
- `peri-acp/src/agent/builder_v2.rs` —— 改用新函数

**估算**：~200 行重构

### P5.4 91 测试迁移
**目标**：把 v1 executor 测试迁移到 v2 StageContext 或 `MiddlewareInner`。

**文件**：
- `peri-agent/src/agent/executor/mod_test.rs`
- `peri-agent/src/agent/executor/tool_dispatch_test.rs`
- 其他散落的 v1 测试

**估算**：~500 行测试改写

### P5.5 物理删除 v1
**目标**：删除所有 v1 死代码。

**删除清单**：
- `peri-agent/src/agent/executor/{mod,tool_dispatch,tool_setup,final_answer,llm_step,mod_test,tool_dispatch_test}.rs`
- `peri-agent/src/agent/react.rs` 的 `ReActAgent` 块（保留 `Reasoning` / `ToolCall` / `ToolResult` 类型）
- `peri-agent/src/agent/state.rs` 的 `State` trait + `AgentState`（或收缩为测试辅助）
- `peri-agent/src/messages/message_queue.rs` v1 MessageQueue（v2 已有）
- `peri-agent/src/agent/compact/` v1 compact 实现（v2 已有 `compact_v2.rs`）
- `peri-agent/src/agent/events.rs` v1 AgentEvent 枚举

**验证**：
- `grep -r 'ReActAgent'` 零结果
- `grep -r 'trait Middleware<S'` 零结果
- `grep -r 'use peri_agent::agent::events::AgentEvent'` 零结果
- workspace 全测试通过

**估算**：~400 行删除

### P5 提交计划
建议拆 5–7 个 commit，每个对应一个子阶段。完成后更新 `CLAUDE.md` 移除「双轨」段落。

---

## 优先级排序

| 任务 | 工作量 | 阻塞 | 价值 |
|------|--------|------|------|
| ~~Stage 1 提交~~ | ~~0~~ | ~~verifier 复核~~ | ~~解锁 v2 可用性~~ ✅ |
| ~~Stage 2~~ | ~~3–5 天~~ | ~~无~~ | ~~v2 正确性~~ ✅ |
| ~~Stage 3~~ | ~~1–2 天~~ | ~~无~~ | ~~v2 可观测性~~ ✅ |
| ~~手动 smoke test~~ | ~~0.5 天~~ | ~~Stage 1–3~~ | ~~切默认前置~~ ✅（已穿插在 workflow 验证中） |
| ~~切默认~~ | ~~0.5 天~~ | ~~smoke test~~ | ~~用户可见~~ ✅ |
| P5.1–P5.5 | 4–6 周 | dogfood | 完全消除双轨 |

**推荐顺序**：~~smoke test~~ → ~~切默认~~ → （dogfood 2 周）→ P5.1 → P5.2 → P5.3 → P5.4 → P5.5
