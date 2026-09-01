# Workflow delivery 契约与 Git postcondition 闭环

**状态**：Blocked
**优先级**：高
**类型**：Safety — Workflow 系统
**创建日期**：2026-09-01
**最后核查**：2026-09-01

## 目标

在不把 engine `completed` 冒充 deliverable 的前提下，建立执行、验收、收口、交付四层独立投影；写入型 Workflow 只能依据声明式 ownership 与 Git plumbing 事实判定交付，任何缺失或检查异常均 fail-safe 为 blocked/unknown。

## 已落地基础

- canonical Rust/Node/TUI 类型已加入四层状态；legacy snapshot 缺失字段默认 `unknown`，`completed` 不推出 deliverable。
- `RunProgress → runs_snapshot → TUI DTO/panel` 保留并分别展示 execution、acceptance、post-processing、delivery。
- tool schema 接受 `writeIntent=read_only|write`；strict preflight 在 primitive/graph 无法静态验证时于 run_id 发布、spawn、run 目录、registry/TaskManager/event 之前明确拒绝。
- `GitBaseline` 以 `GIT_OPTIONAL_LOCKS=0` 捕获 canonical repo、cwd、HEAD 与 porcelain v2 facts；生产终态执行 postcondition，不恢复或覆盖用户内容。
- `read_only`/legacy 禁止 Git 事实变化；`write` 校验 canonical repo/cwd、relative allowlist、HEAD policy、commit paths，并在 baseline 已含 staged 内容时拒绝归属 commit。
- resume 在 spawn 前原子 `reserve` registry 槽，拒绝路径不再产生 detached runner。
- journal 无法从 engine callback 证明 agent identity 时省略 `agentId`，不再用 `seq` 伪造；真实一一关联仍需 engine 提供 typed identity。
- `state.json` 写入失败会把对外终态降级为 failed/blocked，通知不再声称 completed。
- fixtures 覆盖 dirty/staged/binary untracked 原样保留、无归属变化、allowlist 内写入、allowlist 越界和 traversal 拒绝。

## WF-03/WF-04/WF-05 实现基础（验收未闭环）

- WF-03：host 对当前可可靠观测的 `maxAgents`、`maxToolCalls`、`maxElapsedMs` 执行 fail-safe 门限；run state 持久化 limits，终态保持既有 `completed/failed/killed`，不新增 paused。
- WF-04：run_id、registry/TaskManager、run directory、agent 之前同步完成 script/cwd/canonical repo、JS-safe limit preflight；script 校验复用打包 artifact 的 `validateScript`，provider/tier 不做网络探活。strict 因 graph/primitive 不能完整静态证明而明确拒绝。
- WF-05：journal 在保留 legacy `key/seq/result` 和 resume cache-hit 的同时，optional attempt 使用完整 `run_id/agent_id/journal_seq` 表达 `recovered_from/consumed/disposition`；recovered entry 写入新 run journal，恢复链不依赖短 ID 或自然语言解析。

## 当前阻塞

现有 `AgentRunResult`/journal 没有 typed changed-files、acceptance finding/disposition，也没有完整 phase-boundary contract。Node engine journal callback 也不提供产生条目的真实 `agentId`。当前 host 可依据 Git before/after facts执行保守 allowlist 与 commit-path 对账，但仍无法安全判断：

- 哪些路径由 agent 声明为本次 run 的预期产物（host 当前仅能对 before/after Git facts 做保守差分）；
- acceptance P1 是否通过；
- HEAD 变化是否由本次 run 声明且符合 parent/commit file list 约束；
- 多 repo、错误 cwd、越界路径、commit 夹带和假 changed-files 的归属。

因此当前实现对缺失 intent 或证据不足的写入型 run 只报告事实并保持 `post_processing=blocked`、`delivery=blocked`；不会猜测或自动修复工作树。

## 可执行验收

关闭前必须增加并通过：

1. Rust/TS roundtrip、旧 JSON、未知字段、`completed != deliverable`。
2. preflight 每种失败：0 agent、0 tool-call、0 run 目录、0 registry/TaskManager/event。
3. dirty、staged、untracked、错误 cwd、多 repo、越界路径、假 changed-files、无实际 diff、意外 HEAD、commit 夹带均 fail-safe，原有内容逐字节不变。
4. acceptance P1 failure 与 post-processing failure 独立保留 execution 证据，并令 delivery blocked。
5. Node → Rust state/result → ACP snapshot/event → Agent Defer/bg completion → TUI 同时显示 engine completed 与 delivery blocked。
6. notification 丢失不影响 `state.json` 权威状态。

## 非目标

本 issue 不引入 orchestration 状态机，不实现 engine phase-boundary pause/resume、阶段预算、完整 graph compile/validate、`maxCompactions` 或 typed finding policy；这些能力在契约具备前保持 blocked，并作为后续 acceptance：engine 必须提供可验证 phase/compaction 边界与消耗事件，host 不得以 wall-clock、agent/tool 计数冒充上述能力。
