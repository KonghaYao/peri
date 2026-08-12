# peri-agent Interaction Multiplex 设计

> 简要说明 | 日期：2026-07-15 | v2

## 1. 用途

MultiplexBroker 是 HITL 审批系统的多路复用器。当系统同时连接多个审批通道时（TUI 终端 + 微信/Slack/飞书等 Channel），MultiplexBroker 将审批请求广播到所有通道，取最先响应的结果。

```
审批请求
  ├─ TUI Broker（本地终端）
  ├─ Channel Broker（微信）
  └─ Channel Broker（Slack）
       ↓ 竞速
  最先响应者胜出 → Approve/Reject
```

## 2. 设计

### 2.1 统一交互架构

#### UserInteractionBroker trait

所有 broker 实现统一的 `UserInteractionBroker` trait（定义已下沉 `peri-acp-types/src/interaction.rs:113`；`peri-agent/src/interaction/mod.rs` 仅 re-export），将 HITL（工具审批）和 AskUser（问答）两条路径统一为单一接口：

```rust
#[async_trait]
pub trait UserInteractionBroker: Send + Sync {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse;
}
```

实现方：`MultiplexBroker`、`ChannelBroker`、`AcpTransportBroker`、`StdioBroker`。

#### 统一交互类型

`InteractionContext` 枚举（`peri-acp-types/src/interaction.rs:48`）描述交互场景：

| 变体 | 含义 |
|------|------|
| `Approval { items: Vec<ApprovalItem> }` | 工具调用前审批（原 HITL BatchApprovalRequest） |
| `Questions { requests: Vec<QuestionItem> }` | 向用户提问（原 AskUserBatchRequest） |

`InteractionResponse` 枚举（`peri-acp-types/src/interaction.rs:86`）描述响应结果：

| 变体 | 含义 |
|------|------|
| `Decisions(Vec<ApprovalDecision>)` | 审批决策（Approve / Reject / Edit / Respond） |
| `Answers(Vec<QuestionAnswer>)` | 问题答案 |
| `Rejected` | 用户明确拒绝交互 |

### 2.2 Broker 类型

#### AcpTransportBroker（ACP RPC broker）

`peri-acp/src/broker/transport_broker.rs` — TUI/ACP 传输层 broker：

- `Approval` → 转为 `session/request_permission` RPC，每个 `ApprovalItem` 发送独立的 `RequestPermission` 请求
- `Questions` → 转为 `elicitation/create` RPC，聚合所有 `QuestionItem` 为单个表单 schema

#### StdioBroker（stdio 自动审批 broker）

`peri-acp/src/host/stdio/context.rs:85`（stdio 宿主随 L5 迁入 peri-acp，原 `peri-tui/src/acp_stdio/` 目录已不存在）— stdio 传输模式下的默认 broker：

- 对所有 `Approval` 自动返回 `Approve`
- 对 `Questions` 返回空答案

#### ChannelBroker（MCP Channel broker）

`peri-agent/src/interaction/channel_broker.rs` — 通过 MCP Channel 发起权限审批：

- `Approval`：向所有已授权 channel 发送 `permission_request` 通知，在 `pending_permissions` 中注册 oneshot 等待响应，5 分钟超时
- `Questions`：返回空答案（Channel 不支持交互式问答）

##### ChannelState 共享状态

`peri-agent/src/interaction/channel_state.rs` — 单一实例，由 `ServiceRegistry` 持有：

| 字段 | 类型 | 用途 |
|------|------|------|
| `authorized` | `RwLock<HashMap<String, String>>` | 已授权 server → source 映射 |
| `pending_permissions` | `Mutex<HashMap<String, oneshot::Sender<PermissionResponse>>>` | 待审批的权限请求 |
| `channel_msg_txs` | `RwLock<HashMap<String, UnboundedSender<ChannelNotification>>>` | session 消息发送器注册表 |

##### ChannelNotificationSender trait

`peri-acp-types/src/interaction.rs:122` — 发送 channel 通知的抽象，由 `McpClientPool` 实现：

```rust
#[async_trait]
pub trait ChannelNotificationSender: Send + Sync {
    async fn send_notification(&self, server_name: &str, method: &str, params: Value) -> Result<(), String>;
}
```

##### MCP Channel 协议类型

`peri-agent/src/interaction/channel_types.rs`：

| 类型 | 方向 | 用途 |
|------|------|------|
| `PermissionRequest` | Peri → MCP | 权限请求（request_id / tool_name / arguments / source） |
| `PermissionResponse` | MCP → Peri | 权限响应（request_id / approved / reason） |
| `ChannelNotification` | MCP → Peri | 消息通知（source / chat_id / text） |
| `short_request_id()` | — | UUID v7 前 6 位 hex，用户手打 `yes <id>` 使用 |

### 2.3 MultiplexBroker 竞速机制

- **竞速机制**：所有 broker 通过 `tokio::spawn` 并行执行，首个响应通过 mpsc channel 返回
- **CancellationToken 取消**：首个响应到达后调用 `cancel.cancel()`，其余 spawned task 通过 `select!` 立即取消退出
- **空/单 broker 优化**：0 个 broker 时返回空 `Decisions`；1 个 broker 时跳过 spawn + mpsc，直接调用
- **来源标记**：返回的 `ApprovalDecision` 标记 `source` 字段，外部可区分响应来源

### 2.4 Builder 构造逻辑

`peri-middlewares/src/assembly.rs:158-178`（L5 后装配点自 `peri-acp/src/agent/builder.rs` 迁入，原文件不存在）：

- 当 `channel_state` 和 `mcp_pool` 同时存在时，将 TUI broker（`AcpTransportBroker`）与 Channel broker 包装为 `MultiplexBroker`
- 否则直接使用 TUI broker
- `AskUserTool` 始终绕过 `MultiplexBroker`，使用原始 TUI broker（`assembly.rs:177-180`），避免 Channel 空答案竞速胜出导致弹窗被绕过

### 2.5 HITL 中间件增强

`peri-middlewares/src/hitl/mod.rs`：

- **SharedPermissionMode**：`Always`（全部审批）/ `Ask`（全部询问）/ `Auto`（LLM 自动分类）
- **AutoClassifier**：`LlmAutoClassifier` 基于 LLM 判断工具调用是否需要审批，含缓存 TTL

## 3. 约束

- `AskUserTool` 绕过 `MultiplexBroker`：`ChannelBroker` 处理 `Questions` 时返回空答案，`MultiplexBroker` 竞速时 Channel 总是先返回，导致 `AskUserQuestion` 弹窗被绕过。因此在 builder 中 `AskUserTool` 直接使用原始 TUI broker
- 使用 `CancellationToken` 在首个响应后取消其余 broker
