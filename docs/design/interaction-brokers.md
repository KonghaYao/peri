# 交互 Broker 与审批、问答设计

> 状态：现行设计
>
> 跨 transport 的结算、串行化与关闭语义以 ARC-HITL-001 和
> ARC-TRANSPORT-001 为准。

## 1. 用途

`UserInteractionBroker` 统一工具审批与用户问答。ACP 宿主通过 transport broker
与客户端交互。channel 消息、权限通知、共享状态及随其失去生产用途的多路竞速组合器
已退役，不保留待未来启用的实现。

## 2. 设计

### 2.1 统一交互架构

#### UserInteractionBroker trait

所有 broker 实现统一的 `UserInteractionBroker` trait（定义已下沉 `peri-acp-types/src/interaction.rs`；`peri-agent/src/interaction/mod.rs` 仅 re-export），将 HITL（工具审批）和 AskUser（问答）两条路径统一为单一接口：

```rust
#[async_trait]
pub trait UserInteractionBroker: Send + Sync {
    async fn request(
        &self,
        owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse;
}
```

`owner` 由调用方从真实执行事实铸造；无法给出可信归属的调用方必须显式拒绝，不得传入伪造归属。生产实现为 `AcpTransportBroker`，TUI 与 stdio 宿主共用同一交互语义。

#### 统一交互类型

`InteractionContext` 枚举（`peri-acp-types/src/interaction.rs`）描述交互场景：

| 变体 | 含义 |
|------|------|
| `Approval { items: Vec<ApprovalItem> }` | 工具调用前审批（原 HITL BatchApprovalRequest） |
| `Questions { requests: Vec<QuestionItem> }` | 向用户提问（原 AskUserBatchRequest） |

`InteractionResponse` 枚举（`peri-acp-types/src/interaction.rs`）描述响应结果：

| 变体 | 含义 |
|------|------|
| `Decisions(Vec<ApprovalDecision>)` | 审批决策（Approve / Reject / Edit / Respond） |
| `Answers(Vec<QuestionAnswer>)` | 问题答案 |
| `Rejected` | 用户明确拒绝交互 |
| `Unanswered { cause: UnansweredCause }` | 无人可作答：客户端（如 `-p` 打印模式）声明正常收到提问但无法提供交互界面，或本端没有可投递的在线 endpoint。`cause` 区分客户端声明（`NonInteractiveClient`、无法识别的 `Unknown`）与本端事实（`NoOnlineEndpoint`、`OwnerUnmatched`、`AttachmentDetached`）。工具侧以失败结果如实转述，不伪造空答案 |

### 2.2 Broker 类型

#### AcpTransportBroker（ACP RPC broker）

`peri-acp/src/broker/transport_broker.rs` — TUI/ACP 传输层 broker：

- `Approval` → 转为 `session/request_permission` RPC，每个 `ApprovalItem` 发送独立的 `RequestPermission` 请求
- `Questions` → 转为 `elicitation/create` RPC，聚合所有 `QuestionItem` 为单个表单 schema

默认审批模式为 `Forward`，stdio 不自动批准全部工具。`AutoApprove` 必须显式选择，
是 broker 的本地决策模式，不是另一套 stdio 实现。问答取消携带
`peri.elicitationUnanswered` 原因时返回 `Unanswered`；无该声明的生命周期取消
按既有空答案结算，不把无人可作答当作用户回答。

### 2.3 Builder 构造逻辑

`peri-middlewares/src/assembly/` 的装配函数直接使用宿主传入的 broker，
不根据 MCP pool 构建额外通知审批路径。Permission 与 AskUser 都使用明确的
用户交互端口；MCP 标准 elicitation、Apps relay 与资源订阅不依赖 channel。

### 2.4 审批与提问的中间件分工

- 审批：`PermissionMiddleware`（`peri-middlewares/src/permission/`）消费
  `UserInteractionBroker`；`PermissionMode` 为 `Default` / `AcceptEdit` /
  `AutoMode` / `Bypass`，`SharedPermissionMode` 是共享状态封装，契约定义在
  `peri-acp-types/src/permission.rs`。
- 提问：`HumanInTheLoopMiddleware`（`peri-middlewares/src/hitl/mod.rs`）只持有
  `AskUserQuestion` 工具，不再承担审批钩子。
- `LlmAutoClassifier`（`peri-middlewares/src/permission/auto_classifier.rs`）实现
  `AutoMode` 的工具调用分类，含缓存 TTL。

## 3. 约束

- 审批和问答的取消、关闭与一次性结算遵循 ARC-HITL-001，不增加隐藏的外部通知审批路径。
