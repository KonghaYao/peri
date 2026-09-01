# 有历史时快速输入的首次请求缓存命中率偏低

**状态**：Fixed
**优先级**：中
**创建日期**：2026-09-01

## 问题描述

会话已有历史时快速输入 prompt，第一次模型请求显示约 70% prompt cache coverage，后续请求恢复正常。二次审计确认环境为 **OpenAI-compatible 接口 + Cursor provider**；adapter 请求构造本身无首轮特判，问题来自本地 cold restore 前缀漂移与普通 thread load 的快速输入竞态。

来源：[GitHub #113](https://github.com/KonghaYao/peri/issues/113)

## 症状详情

- 首次请求显示约 70% cache ratio；旧 UI 将 `cached / total input` 错称为“命中率”。
- 同一任务的后续请求正常。
- 用户说明问题复现率较低。

## 复现条件

- **复现频率**：偶发。
- **触发步骤**：
  1. 打开包含历史消息的会话。
  2. 快速输入并提交 prompt。
  3. 观察第一次模型请求的 prompt cache 命中率。
- **环境**：Peri TUI；OpenAI-compatible 接口；Cursor provider。

## 涉及文件

- `peri-model/src/anthropic/cache.rs` —— provider payload 的 cache breakpoint 生成。
- `peri-agent/src/agent/model_bridge.rs` —— agent 消息、system 与 tools 到 ModelRequest 的投影。
- `peri-agent/src/session/exec/stage_builder.rs` —— frozen system 与动态 middleware contribution 组装。
- `peri-agent/src/session/exec/executor_helpers/v2_execute.rs` —— root EventBus forwarder terminal barrier。
- `peri-tui/src/kit/acp_notifier.rs` / `acp_events/turn.rs` —— root-only final cache coverage 聚合与展示。
- `peri-acp/src/session/frozen_snapshot.rs` / `host/requests/session_lifecycle.rs` —— frozen owner state 持久化与 cold restore。
- `peri-acp-types/src/store.rs` / `peri-resources/src/sessions/{sqlite_store,filesystem}.rs` —— snapshot 存储契约与实现。
- `peri-tui/src/acp_client/client.rs` / `kit/thread_load_consumer.rs` ——普通 load 的同步 reservation 与 prompt 线性化。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-01 | — | Open | agent | 从 GitHub #113 建立 active spec |
| 2026-09-01 | Open | Fixed | agent | 修复 terminal usage 顺序与最终 coverage 聚合；修复 Anthropic dynamic seam/tool-result breakpoint |
| 2026-09-01 | Fixed | Open | agent | observability 与 Anthropic 子问题已修复，但原 issue 的 provider absolute cached-token 行为尚未识别，不满足关闭条件 |
| 2026-09-01 | Open | Fixed | agent | 用户补充 OpenAI-compatible + Cursor；二次静态审计确认 cold restore 前缀漂移和普通 load 快速输入竞态，并完成修复与全量回归 |

## 修复记录

- Provider-neutral：主 turn 在 EventBus forwarder 排空后才发布终态，保证 final root `LlmCallEnd/UsageUpdate` 先于 `TurnDone`；forwarder 失败走 ordered `AgentExecutionFailed`，不使用 fail-open timeout。
- TUI：只记录未带 `sourceAgentId` 的 root usage；每 step 不再告警，TurnDone 使用最后一次 root observation 至多提示一次。first low、final healthy 时不提示；final missing/zero/invalid 会显式清除较早样本，不推断 provider 失败。
- 文案：指标改为 cache coverage，并展示 `cached / input`、uncached 绝对 token 与 request id。
- Anthropic：空/缺失 frozen base 加 request-time contribution 时仍有显式 uncached seam；工具结果可成为 cache breakpoint，append-only 工具循环能推进到最新 result。
- Cold restore：session/new 持久化版本化 frozen snapshot；load/resume 复用创建时的 system/date/CLAUDE/skills/MetaHarness，legacy thread 缺失时按 ThreadMeta.cwd 原子 write-once 回填、并发 loser 重读 winner；未知/损坏版本及 metadata/store 错误 fail closed 且不覆盖；fork 继承 source snapshot；new/fork 写失败补偿删除。
- 快速 thread switch：ThreadLoadDispatcher 在 channel send 前同步取得引用计数 reservation；ensure_session、prompt、prompt_with_bg_results 在 reservation mutex 下线性化 Stable/open prompt lease；send failure 不外泄 guard，request 丢弃与 in-flight shutdown 均释放。compact 重放先 reserve，再 drain buffered input。
- Cursor/OpenAI wire：未发现首轮 body/header 特判；同一 OpenAI-compatible request builder 用于首次、后续与 retry。未添加 Cursor 未文档化的私有 header 或无保证的 cache key。

## 验证与残余

- `cargo test -p peri-acp --lib`：583 passed。
- `cargo test -p peri-tui --lib`：1358 passed，1 ignored。
- `cargo test -p peri-resources --lib`：53 passed。
- MCP/tools 从 Pending 变 Ready 仍可能改变动态 suffix；在没有明确 readiness 契约前不以猜测性等待修复。
- `AgentPool` fingerprint 未覆盖 endpoint/key 等完整 provider identity、多个 buffered prompt 的 FIFO 执行顺序是独立问题，不纳入 #113。
