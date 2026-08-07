# peri-agent

Rust Agent 框架，实现 v2 `run_react_loop`（ReAct 循环 + EventBus 三层事件 + MessageTranscript 持久化）与可组合中间件系统。

## v2 架构概览

```
run_react_loop（peri-agent::agent::stages）
  └─ 每轮：Compact → Receive → Reason → Act → End
       ├─ Reason: before_model → LLM → after_model
       ├─ Act:    before_tools_batch → 并发 invoke → after_tool × N → after_tools_batch
       └─ End:    检查 MessageQueue 是否有待处理 Prompt/Defer，决定是否续跑下一轮
```

主入口：

- `peri_agent::agent::stages::run_react_loop(context: StageContext, max_iterations: usize)` —— v2 单路径 ReAct 循环
- `StageContext` —— 持有 `llm` / `shared_tools` / `middleware_chain` / `transcript` / `event_bus` / `compact_config` 等
- `EventBus` —— 三层事件（Render / State / Observe），订阅者按需消费
- `MessageTranscript` —— 会话级权威消息存储，标记代替删除（truncated / excluded）

## 快速开始

```rust,ignore
use peri_agent::prelude::*;
use peri_agent::agent::stages::{run_react_loop, StageContext, NullReactLLM};
use std::sync::Arc;
use parking_lot::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _guard = peri_agent::telemetry::init_tracing("my-agent");

    // 1. 构造 LLM（实现 ReactLLM trait，示例用 NullReactLLM 占位）
    let llm: Arc<dyn ReactLLM + Send + Sync> = Arc::new(NullReactLLM);

    // 2. 构造 middleware chain（示例：仅 LoggingMiddleware）
    let mut chain = MiddlewareChain::new();
    chain.push(Box::new(LoggingMiddleware::new().verbose()));

    // 3. 构造 TurnContext + 共享 transcript/queue，通过 StageContext::builder 装配
    let ctx: StageContext = StageContext::builder(
        turn_context,
        Arc::new(RwLock::new(MessageTranscript::new())),
        MessageQueue::new(),
    )
        .with_llm(llm)
        .with_middleware_chain(Arc::new(chain))
        .build();

    // 4. 进入 v2 循环
    let result = run_react_loop(ctx, 10).await;

    println!("完成");
    Ok(())
}
```

## 核心概念

### StageContext 与 run_react_loop

`StageContext` 是 v2 单路径循环的运行时上下文。`run_react_loop` 接管整个 ReAct 循环：

- **Compact 阶段**：检查 `ContextBudget`，按需触发 micro / full compact（`compact_v2::run_compact`）
- **Receive 阶段**：从 `MessageQueue` 取出 Prompt + Info 消息，写入 `MessageTranscript`
- **Reason 阶段**：`before_model` → 调 LLM → `after_model`，emit `LlmCallStart` / `LlmCallEnd`
- **Act 阶段**：工具分发（`before_tools_batch` → 并发 invoke → `after_tool` × N → `after_tools_batch`）
- **End 阶段**：检查 `MessageQueue` 是否有 Defer/Prompt，决定是否续跑下一轮

### 中间件（Middleware）

通过实现 `Middleware` trait 在 Agent 生命周期各节点插入逻辑。v2 中间件接收 `&MiddlewareContext`（只读）或 `&mut MiddlewareContextMut`（可变），不再泛型于 `<S: State>`。

```rust,ignore
use async_trait::async_trait;
use peri_agent::prelude::*;

struct MyMiddleware;

#[async_trait]
impl Middleware for MyMiddleware {
    fn name(&self) -> &str { "my-middleware" }

    async fn before_agent(&self, ctx: &MiddlewareContext<'_>) -> AgentResult<()> {
        // Agent 开始前执行（只读观察）
        Ok(())
    }

    async fn before_model(&self, ctx: &mut MiddlewareContextMut<'_>) -> AgentResult<()> {
        // 每轮 LLM 调用前执行（可变）
        Ok(())
    }

    async fn before_tool(&self, ctx: &mut MiddlewareContextMut<'_>, call: &ToolCall) -> AgentResult<ToolCall> {
        // 工具调用前执行，可修改调用参数
        Ok(call.clone())
    }

    async fn after_tool(&self, ctx: &mut MiddlewareContextMut<'_>, _: &ToolCall, _: &ToolResult) -> AgentResult<()> {
        // 工具调用后执行
        Ok(())
    }
}
```

生命周期钩子执行顺序：`before_agent` → 每轮（`before_model` → `after_model` → `before_tools_batch` → `after_tool` × N → `after_tools_batch`）→ `after_agent`，出错时触发 `on_error`。

### 自定义工具（Tool）

```rust,ignore
use async_trait::async_trait;
use peri_agent::tools::BaseTool;

struct EchoTool;

#[async_trait]
impl BaseTool for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "原样返回输入内容" }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            },
            "required": ["message"]
        })
    }

    async fn invoke(&self, input: serde_json::Value) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(input["message"].as_str().unwrap_or("").to_string())
    }
}
```

### 事件回调（EventBus）

v2 通过 `EventBus` 发出三层事件（发射统一 v2 形态，v1 `ExecutorEvent` 中间态已退役）：

- **RenderEvent** —— LLM 调用 / 工具调用（UI 渲染层消费）
- **StateEvent** —— 消息追加 / 状态变更 / Todo / MessagesCompacted（持久化层消费）
- **ObserveEvent** —— 可观测性事件（langfuse / metrics，身份透传）

`ExecutorEvent`（`peri_agent::agent::events::ExecutorEvent`）不再由 Agent 层发射——仅保留为 ACP 协议序列化面载体（由 `event_v2::*_event_to_executor` 从 v2 事件转换，wire format 不变）。`AgentEventHandler` / `FnEventHandler` 是 ACP 协议化接收端接口：

```rust,ignore
use std::sync::Arc;
use peri_agent::prelude::*;

let handler = FnEventHandler(|event| match event {
    ExecutorEvent::ToolStart { name, .. } => println!("开始调用工具: {name}"),
    ExecutorEvent::ToolEnd { name, is_error, .. } => println!("工具 {name} 完成，错误={is_error}"),
    ExecutorEvent::TextChunk(text) => println!("回答: {text}"),
    _ => {}
});
```

## Telemetry（可观测性）

### 基本用法

在 `main` 入口调用一次，其余自动处理：

```rust,ignore
let _guard = peri_agent::telemetry::init_tracing("my-agent");
// _guard 必须存活到程序退出，drop 时自动 flush
```

### 开关控制

**不配置环境变量则不开启 OTLP**，仅输出到 stdout：

| 环境变量                      | 说明                                            |
| ----------------------------- | ----------------------------------------------- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | 设置后自动启用 OTLP 导出，未设置则只输出 stdout |
| `RUST_LOG`                    | 日志级别，默认 `info`                           |
| `RUST_LOG_FORMAT=json`        | 使用 JSON 格式输出（默认 pretty）               |

```bash
# 仅 stdout 输出（默认行为）
cargo run

# 开启 OTLP 导出到本地 Jaeger
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run --features otel

# 调整日志级别
RUST_LOG=debug cargo run
RUST_LOG=peri_agent=trace cargo run
```

### 本地可视化（Jaeger）

项目根目录提供了 `docker-compose.otel.yml`，一键启动 Jaeger（内置 OTLP 接收器 + UI）：

```bash
# 启动
docker compose -f docker-compose.otel.yml up -d

# 停止
docker compose -f docker-compose.otel.yml down
```

启动后：

- **可视化 UI**：<http://localhost:16686>
- **OTLP HTTP**：`http://localhost:4318`（`OTEL_EXPORTER_OTLP_ENDPOINT` 填这个）
- **OTLP gRPC**：`localhost:4317`

### otel Feature

OTLP 导出功能通过 Cargo feature 控制，默认不编译进二进制：

```toml
# Cargo.toml
[dependencies]
peri-agent = { version = "*", features = ["otel"] }
```

| 场景                     | 配置                                              | 结果                        |
| ------------------------ | ------------------------------------------------- | --------------------------- |
| 开发/测试                | 无                                                | 只输出到 stdout             |
| 生产（有 Collector）     | `OTEL_EXPORTER_OTLP_ENDPOINT` + `--features otel` | 同时导出 trace              |
| 配置了变量但未开 feature | `OTEL_EXPORTER_OTLP_ENDPOINT`（无 feature）       | 打印 warn，降级为 stdout    |
| OTLP 初始化失败          | 网络不通等                                        | 打印 warn，自动降级，不崩溃 |

`run_react_loop` 内的 LLM 调用、每次工具调用均已自动埋点，无需额外代码。

## Cargo Features

| Feature | 默认 | 说明                                                                                           |
| ------- | ---- | ---------------------------------------------------------------------------------------------- |
| `otel`  | 否   | 启用 OpenTelemetry OTLP 导出（`opentelemetry`、`opentelemetry-otlp`、`tracing-opentelemetry`） |
