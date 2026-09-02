# 长上下文中每轮缓存命中率持续下降

**状态**：Reopen
**优先级**：高
**创建日期**：2026-09-01

## 问题描述

长上下文工具任务中，每一轮模型请求都显示低于 80% 的 prompt cache 比例，并从约 70% 持续下降到 67%。静态分析证明该比例是 `cached_tokens / input_tokens` coverage：固定 cached 绝对值与增长的工具 suffix 即可精确产生 70→69→68→67，不能据此断言 cache 被驱逐。

来源：[GitHub #114](https://github.com/KonghaYao/peri/issues/114)

## 症状详情

- 同一长任务中多轮依次显示 70%、69%、68%、67% cache hit。
- 多轮告警显示 `chatcmpl-*` request id，支持该记录来自 OpenAI-compatible 路径，而非 Anthropic 显式 breakpoint 路径。
- 用户补充：重启之后缓存恢复正常。

### 验证 #1（2026-09-02）—— Reopen

用户反馈修复后缓存率提示完全不可见，怀疑当前展示门控或计算公式错误。

## 复现条件

- **复现频率**：偶发；长上下文下观察到连续复现。
- **触发步骤**：
  1. 在长历史会话中执行多轮 shell/read/edit 等工具调用。
  2. 让 agent 持续进行多个 Reason/Act 循环。
  3. 观察每次模型请求的 prompt cache 命中率。
  4. 重启 Peri 后对比缓存状态。
- **环境**：Peri TUI；issue 日志含 shell、glob、grep、edit 工具；具体模型与 provider 配置未提供。

## 涉及文件

- `peri-model/src/openai_compatible/request.rs` —— prepared body 的稳定 system/tools/message prefix。
- `peri-model/src/anthropic/cache.rs` —— 并行静态分析发现并修复的 tool_result breakpoint 缺陷（不是 #114 `chatcmpl-*` 主路径证据）。
- `peri-agent/src/agent/model_bridge.rs` —— transcript 到 ModelRequest 的投影。
- `peri-agent/src/session/exec/executor_helpers/v2_execute.rs` —— final usage 的 EventBus completion barrier。
- `peri-tui/src/kit/acp_notifier.rs` / `acp_events/turn.rs` —— coverage 聚合与一次性告警。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-01 | — | Open | agent | 从 GitHub #114 建立 active spec |
| 2026-09-01 | Open | Fixed | agent | 将误导的逐 step “命中率/驱逐”告警改为有终态顺序保证的 final coverage |
| 2026-09-02 | Fixed | Reopen | user | 修复后缓存率提示完全不可见，需要重新核对展示门控与公式 |

## 修复记录

- OpenAI prepared-body 回归测试锁定 N→N+1 工具轮：system/tools 完全相同，每个既有 message element 的序列化 bytes 完全相同；未添加未经 provider 文档验证的 request 字段。
- root EventBus forwarder 变为可 await 的 completion barrier。真实 gated integration test 证明 final `LlmCallEnd` 被延迟时 session 不会提前 `AgentDone`；release 后 usage 严格早于 done。JoinError 发 ordered `AgentExecutionFailed` 并使 turn 失败。
- child/workflow `LlmCallEnd.source_agent_id` 经 ACP `_meta.peri.sourceAgentId` 透传；父 TUI 忽略 auxiliary usage，避免迟到低 coverage 覆盖 root final sample。
- TUI 只在 TurnDone 使用 latest root sample，至多产生一条 coverage note；展示 cached/input/uncached 绝对 token，不再从比例宣称 eviction。

## 验收结论与残余

- 本修复关闭的是错误观测语义与 terminal ordering hole；不宣称 provider 绝对 cached token 增加，也不把重启解释为服务端 cache 恢复。
- 正常 append-only OpenAI prepared request 未发现本地 prefix drift。若未来 absolute cached tokens 确实逐轮下降，须带 provider 原始响应与 prepared bodies 另开上游/provider 调查。
- 多 buffered prompt 并发执行是独立残余，不在本 issue 修改。
