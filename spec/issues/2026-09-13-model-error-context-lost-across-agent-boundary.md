# ModelError 安全 typed 上下文跨 Agent 边界丢失

**状态**：Open  
**优先级**：中  
**类型**：缺陷 / 错误契约 / 安全诊断  
**创建日期**：2026-09-13  
**来源**：本轮用户授权记录；观察队列 `PERI-20260913-LLM-ERROR-CONTEXT`

## 问题描述

`ModelError` 在 model runtime 内部保留了受限的错误类别、HTTP status、provider 和 request identity；经过 retry observer、Agent model bridge 和上层错误映射后，部分信息变成字符串或只保留通用错误文本。SubAgent 恢复和 ACP 事件因此可能只能从显示文本推断请求诊断，难以可靠区分临时中断、可重试服务错误和请求/配置拒绝。期望在安全投影边界内保留恢复分支真正需要的 typed 事实，同时继续禁止原始 provider body 和现有 retry 状态机泄漏。

## 症状详情

- 观察队列记录 14 个显式 Agent error result、6 个受影响 thread；分母为同一 48 roots 样本中的 24 个 Agent 显式错误，不能解释为总体比例。
- 研究证据定位：thread `01a07e86-72ed-7453-846a-edc2790c2402` 的 messages `01a07e8a-3e88-7520-9261-33fdd66f8722`、`01a07e8b-e21e-7633-88b5-3aa1931ca853`；calls `call_Cjb90VgCP9es914P1qJfhMDP`、`call_L9Oc5R6SInokBD2shrM5nb6J`。
- `peri-model/src/runtime/error.rs:175-203` 的 `ModelErrorInner` 保留 transport/http/protocol/cancel/stream/retry exhausted 变体；`:270-310` 提供 status/provider/request_id 访问器，`:313-351` 通过脱敏 Display 输出。
- `peri-model/src/runtime/retry.rs:58-82` 的默认 retry 分类只覆盖 transport、408、429、500..=599 和部分 protocol；`:157-204` 的 `RetryObservation` 对上层只暴露 attempt、delay 和 `RetryErrorKind`。
- `peri-agent/src/agent/model_bridge.rs:627-637` 将 `ModelError` 映射为 `AgentError::LlmHttpError { status, message: error.to_string() }` 或字符串 `LlmError`；`AgentError::user_facing_message` 在 `peri-acp-types/src/error.rs:56-72` 对 LLM 错误使用通用脱敏消息。
- `peri-agent/src/session/retry_events.rs:16-35` 将 retry observation 投影为 ACP `LlmRetrying` 事件，只携带错误类别字符串；它与父 Agent resume 不是同一件事。

## 现行观察与待验证假设

### 现行观察

- runtime 的 typed 安全字段存在，但至少有一条 model→Agent→event/user-facing 路径将其压缩为字符串或通用文本。
- 当前 400/401/403/404 不在默认 retryable HTTP 集合中；代码审计和现有测试覆盖的是 retry 行为，不足以证明 SubAgent 恢复所需诊断在所有边界都可用。
- 请求 ID 和 provider 已经过 `SafeErrorContext` 长度/字符约束；任何扩展必须继续沿用安全投影，不能直接输出原始请求或响应内容。

### 待验证假设

- 某些恢复分支确实需要 status、错误类别或 request identity，而当前显示文本不足以稳定判断；也可能现有 ACP/Agent 结果已经足够，需用端到端 fixture 先证伪或确认。
- typed 投影若只在 Agent→SubAgent/ACP 边界增加窄字段，可能改善恢复判断而不改变 retry；不能先假定必须扩展 `ModelError` 或把 400 纳入重试。

这些边界投影与历史错误计数存在相关性，但尚未证明它是任何特定恢复失败的根因。

## 验收场景

1. 将 400、429、500、可见 delta 后 stream interruption 四类 fixture 从 model runtime 追到 SubAgent 结果和 ACP 事件，核对类别/status/request identity 是否在需要的安全范围内可判断。
2. 400/401/403/404 的默认不重试策略保持不变；429/500 和既有 transport/protocol retry 行为不回归，取消仍不会被当作失败重试。
3. 仅允许经过验证的 status、类别、受限 request identity 或安全摘要跨边界；不导出原始 provider body、headers、prompt、response 或 secrets。
4. 若现有字段已足够，记录无需扩展的证据并关闭扩展方案；若不足，恢复分支必须依据 typed 事实而不是对显示文本做脆弱匹配。

## 范围边界

- 范围限于 `ModelError` 的安全投影、retry observation、Agent model bridge、SubAgent 恢复结果和 ACP 事件消费。
- 不改变默认 retry 分类，不让 prompt 接管 retry 状态机，不在本 issue 中修复所有 ACP turn-error 展示问题；ACP 通用错误边界已有相邻 issue。
- 不输出原始 provider body 或内部 telemetry，不凭单个 HTTP 400 推断是配置、endpoint、schema 还是请求体根因。

## 复现条件

- **复现频率**：待四类生产路径 fixture 验证；当前为审计样本观察。
- **触发步骤**：
  1. 注入具有安全 status/request identity 的 400、429、500 或 stream interruption。
  2. 观察 model retry、Agent error mapping、SubAgent 恢复输出和 ACP event。
  3. 对照 retry 次数、typed 字段和用户可见文本，确认恢复分支可否不解析自由文本。
- **环境**：Peri model runtime、Agent bridge、SubAgent session、ACP event path。

## 涉及文件

- `peri-model/src/runtime/error.rs` —— ModelError 变体与安全上下文。
- `peri-model/src/runtime/retry.rs` —— retry 分类与安全 RetryObservation。
- `peri-model/src/runtime/retry_test.rs` —— 现有 retry 行为测试。
- `peri-agent/src/agent/model_bridge.rs` —— ModelError 到 AgentError 的边界映射。
- `peri-acp-types/src/error.rs` —— AgentError 与 user-facing 脱敏消息。
- `peri-agent/src/session/retry_events.rs` —— retry 事件跨 session/ACP 投影。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-13 | — | Open | agent | 依据本轮用户授权和观察队列创建；待实际执行验证 |

## 修复记录

（由 auto-issue-fixer 修复阶段追加，创建时留空）
