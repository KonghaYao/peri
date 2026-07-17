# /login 和 /model 面板切换后不立即生效，需重启

**状态**：Fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-16

## 问题描述

在 `/login` 面板切换 Provider 或在 `/model` 面板切换模型别名后，修改被写入了 `PERI_CONFIG_HANDLE` 并持久化到磁盘，状态栏也显示新值，但**实际发起的 LLM 调用仍使用旧配置**。

- `/login` 切换 Provider 后，下一轮对话报认证失败——因为 agent 仍用旧 Provider 的 API Key 发起请求
- `/model` 切换模型别名后，下一轮对话仍使用旧模型

需要**重启 Peri** 才能让切换生效。

## 症状详情

| 面板 | 操作 | 当前行为 | 期望行为 |
|------|------|---------|---------|
| `/login` | Enter 切换 provider | 状态栏显示新 provider，但实际 API 调用仍用旧 provider 的凭据，导致认证失败 | 切换后下一轮对话立即使用新 provider |
| `/model` | Enter 切换 alias | 状态栏显示新 alias，但实际 LLM 调用仍用旧模型 | 切换后下一轮对话立即使用新模型 |

## 复现条件

- **复现频率**：必现（100%）
- **触发步骤**：
  1. 启动 Peri，确认当前 Provider A 可用
  2. 输入 `/login`，Enter 切换到 Provider B
  3. 面板关闭，状态栏显示 Provider B
  4. 发送一条消息
  5. 实际调用仍使用 Provider A 的凭据，报认证失败（Provider B 和 A 的 API Key 不同）
  6. 重启 Peri，切换才生效
- **环境**：macOS，TUI 模式

## 涉及文件

- `peri-tui/src/kit/panels/model.rs:431-450` —— `switch_alias()`：写了 PERI_CONFIG_HANDLE 和 SERVICE_SNAPSHOT，未将新配置推送到运行中的 agent 会话
- `peri-tui/src/kit/panels/login.rs:208-253` —— Enter 事件处理：写了 PERI_CONFIG_HANDLE.active_provider_id 和 SERVICE_SNAPSHOT，未将新配置推送到运行中的 agent 会话
- `peri-tui/src/kit/panels/login.rs:485-513` —— `activate_provider()`：仅做持久化保存，无会话级配置推送

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-16 | — | Open | agent | 创建 |
| 2026-07-17 | Open | Fixed | agent | 修复完成，见修复记录 |

## 修复记录

### 根因

两个独立问题：

**① 面板未推送配置到 ACP 服务端（主 Agent）**
`login/model` 面板只写了 `PERI_CONFIG_HANDLE`（共享 `Arc<RwLock<PeriConfig>>`），但 `prompt_exec.rs:58` 每次执行读取的是 `ctx.provider`（独立 `RwLock<LlmProvider>`），而非从 `peri_config` 重新构建。且 `agent_pool` 缓存的 LLM 实例未失效。

已有正确模式 `CycleProvider`（`submit_consumer.rs:194-212`）调用 `client.update_config()` → ACP 服务端 `handle_update_config` 会同时重建 provider + invalidate pool。

**② Workflow 工具内的子 agent 使用会话级旧快照**
`WorkflowAgentContext.provider` 是裸 `LlmProvider` 值（非共享 Arc），在 `session/new` 时一次性捕获。后续 `builder.rs:403` 优先使用会话级 `WorkflowMiddleware`（含旧 executor），忽略 per-prompt 新 executor。且 workflow agent 不走 `AgentPool`。

### 修复方案

**修复 ①：面板增加 update_config 调用**

- `login.rs` Enter 处理器 → 已有 `tokio::spawn` 块内追加 `client.update_config(&snap).await`
- `model.rs` `switch_alias()` 末尾 → 新增 `tokio::spawn` 块调用 `client.update_config()`

**修复 ②：WorkflowAgentContext.provider 改为共享 Arc<RwLock<>>**

- `workflow_agent.rs`: `WorkflowAgentContext.provider` 从 `LlmProvider` 改为 `Arc<RwLock<LlmProvider>>`；`execute()` 中所有读取加 `.read()`
- `acp_server/requests.rs` (session/new): 使用 `Arc::clone(&cfg.provider)` 共享同一 Arc
- `acp_server/prompt.rs` (per-prompt): 同上
- `acp_stdio/session/create.rs` (session/new): 同上
- `acp_stdio/session/prompt_exec.rs` (per-prompt): 同上
- `acp_stdio/context.rs`: `StdioContext.provider` 从 `RwLock<LlmProvider>` 改为 `Arc<RwLock<LlmProvider>>`（`Arc<T>` Deref 保证所有现有读写代码无需修改）
- `acp_stdio/init.rs`: 构造时加 `Arc::new()` 包裹

### 修改文件清单

| 文件 | 改动 |
|------|------|
| `peri-tui/src/kit/panels/login.rs` | 新增 `ACP_CLIENT_HANDLE` import；Enter 处理器 tokio::spawn 块追加 update_config 调用 |
| `peri-tui/src/kit/panels/model.rs` | 新增 `ACP_CLIENT_HANDLE` import；switch_alias() 末尾新增 tokio::spawn 调用 update_config |
| `peri-acp/src/agent/workflow_agent.rs` | provider 字段类型改为 `Arc<RwLock<LlmProvider>>`；4 处读取加 `.read()`；create_default_executor 内部包裹 |
| `peri-tui/src/acp_server/requests.rs` | session/new 中 provider_snap → `Arc::clone(&cfg.provider)` |
| `peri-tui/src/acp_server/prompt.rs` | per-prompt workflow executor 使用 `Arc::clone(provider)` |
| `peri-tui/src/acp_stdio/session/create.rs` | session/new 中 provider_snap → `Arc::clone(&ctx.provider)` |
| `peri-tui/src/acp_stdio/session/prompt_exec.rs` | per-prompt workflow executor 使用 `Arc::clone(&ctx.provider)` |
| `peri-tui/src/acp_stdio/context.rs` | provider 类型从 `RwLock<>` 改为 `Arc<RwLock<>>` |
| `peri-tui/src/acp_stdio/init.rs` | 构造时加 `Arc::new()` 包裹 |

### 覆盖度

| 场景 | 修复后 |
|------|--------|
| 普通对话 | ✅ 立即生效（agent_pool.invalidate + provider 重建） |
| SubAgent | ✅ 立即生效（共享 AgentPool） |
| Prediction 预填 | ✅ 立即生效（共享 Arc 每次重读） |
| Workflow 工具内子 agent | ✅ 立即生效（共享 Arc，provider 写入后递归可见） |
| Cron/Channel 触发 | ✅ 立即生效（标准路径） |
