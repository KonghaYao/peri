# 统一 transport pending map：消除 mpsc/stdio 重复逻辑

**状态**：Fixed
**优先级**：低
**类型**：架构改进
**创建日期**：2026-07-20
**来源**：`/tmp/architecture-review-peri-acp-20260720.html` 候选 #6（improve-codebase-architecture 审查 peri-acp）

## Problem Statement

`peri-acp/src/transport/` 中的两个传输实现维护了几乎相同的请求-响应匹配逻辑：

| 文件 | 行数 | 重复内容 |
|------|------|---------|
| `mpsc.rs` | 341 | `PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<...>>>>` + 背景 pump 线程分发 response |
| `stdio.rs` | 300 | **完全相同**的 `PendingMap` + pump 模式 |

两者共享相同的结构：
1. 请求发出时在 `PendingMap` 中插入一个 `oneshot::Sender`（以 request_id 为 key）
2. 背景 pump 线程持续读取 inbound message stream
3. 遇到 Response 时按 request_id 从 `PendingMap` 取出发送方并传回
4. 遇到 Notification 时直接分发
5. 遇到 Request 时通过外部回调处理

**影响**：
- 任何 bug 修复或改进（如超时处理、错误传播）需要在两个文件中各改一遍
- 两个实现的结构体字段几乎相同（仅 channel 方向不同），但定义为两个独立类型
- 新增第三种传输（如 WebSocket）时需要再复制一遍

## 修复记录

### 修复 #1（2026-07-20）
- **操作人**：agent
- **commit**：`a6153771 refactor(acp): 抽取 RequestRouter 统一 transport pending map`
- **修复内容**：提取共享 PendingMap 为 RequestRouter，消除 mpsc/stdio 重复逻辑

## 建议方案

抽取公共请求路由层：

```rust
pub struct RequestRouter {
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<JsonValue>>>>,
}

impl RequestRouter {
    pub fn register(&self, id: i64) -> oneshot::Receiver<JsonValue> { ... }
    pub async fn pump(
        &self,
        mut incoming: impl Stream<Item = IncomingMessage>,
        mut on_request: impl FnMut(IncomingMessage) -> Option<JsonValue>,
    ) { ... }
}
```

每种传输只需提供：
- `Stream<Item = IncomingMessage>`（inbound 数据流）
- `Sink`（outbound 数据流，由各自传输适配）

两种传输结构简化为：
- `MpscTransport`：持有 `RequestRouter` + mpsc channel 对
- `StdioTransport`：持有 `RequestRouter` + stdin/stdout 句柄

## 涉及文件

| 文件 | 操作 |
|------|------|
| `transport/mpsc.rs` | 移除 PendingMap + pump，替换为 RequestRouter |
| `transport/stdio.rs` | 同上 |
| 新增 `transport/router.rs` | `RequestRouter` 公共实现 + 测试 |

## 收益

- **DRY**：删除 ~150 行重复逻辑
- **locality**：pending map 的行为（超时、错误、清理）现在只有一处定义
- **extensibility**：新增传输时只需实现 Stream + Sink，不用关注 request/response 匹配

## 风险

- `Speculative` 级别——改动影响两个核心传输路径，回归风险较高
- `StdioTransport` 在构造函数中启动背景任务，改为 `RequestRouter` 后需要调整启动时机
- 两种传输的 notification 处理逻辑可能有微妙差异，合并时需要仔细对比
- 建议在稳定期作为独立专项任务进行，不与其他重构叠加
