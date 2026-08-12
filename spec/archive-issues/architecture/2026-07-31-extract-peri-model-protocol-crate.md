> 归档于 2026-08-11，原路径 spec/issues/2026-07-31-extract-peri-model-protocol-crate.md

# 抽取 `peri-model` 标准模型协议 crate

**状态**：Fixed
**优先级**：高  
**类型**：架构重构  
**创建日期**：2026-07-31  
**相关历史**：provider adapter trait 计划（2026-08-11 已随归档清理删除；其职责由本 crate 承接）

## 最新情况（2026-08-11）

peri-model crate 已抽取落地：protocol/runtime/transport/openai_compatible/anthropic 五模块

## 目标

从 `peri-agent` 中抽取与模型协议、厂商请求格式、HTTP/SSE 传输和模型调用重试有关的职责，建立独立 crate `peri-model`。

`peri-model` 向上提供完整、稳定且与 Agent 无关的标准模型协议；向下封装 OpenAI-compatible 和 Anthropic 等厂商协议的差异。其设计借鉴 Goose 的 `goose-providers` 分层：可复用的 provider 协议核心独立于 Agent 运行时，具体上层组合与应用配置留在调用 crate。

本重构是 workspace 内的一次性破坏性迁移：所有调用方直接依赖 `peri-model`，不在 `peri-agent::llm` 保留兼容 facade。

## 非目标

- 不迁移 ReAct、`Reasoning`、工具执行、Agent 事件、compact、TokenTracker 或 middleware 生命周期。
- 不迁移 ACP 的应用配置持久化、环境变量解析、模型别名、会话生命周期、实例池、Langfuse bridge 或 TUI 配置界面。
- 首期不提供动态 registry、插件 ABI 或字符串协议到 factory 的运行时注册。
- 不修改 `peri-acp-types` 的 ACP wire DTO 边界；它继续独立于运行时 crate。

## 目标依赖方向

```text
peri-tui ─┐
          ├── peri-acp ──────┐
          │                  ├── peri-model
          │                  └── peri-agent ─── peri-model
          └── peri-middlewares ─┬── peri-agent
                                └── peri-model

peri-model ──X──> peri-agent / peri-acp / peri-middlewares / peri-tui
```

`peri-model` 只能依赖通用 Rust 库；不得引入对上层 Peri crate 的依赖，防止循环依赖与协议泄漏。

## `peri-model` 的责任边界

```text
peri-model
├── protocol/                    # 标准、厂商无关的公开协议
├── runtime/                     # 取消、重试、请求准备与观测
├── transport/                   # HTTP 与 SSE 基础设施
├── openai_compatible/           # OpenAI-compatible adapter
└── anthropic/                   # Anthropic Messages API adapter
```

### 标准协议

`peri-model` 必须拥有自己的、与 Agent 无关的 DTO：

- `Model`：面向上层的模型调用 trait。
- `ModelRequest`、`ModelResponse`。
- `ModelMessage`、`ContentBlock`：包括 system、user、assistant、tool result、text、reasoning 等标准内容语义。
- `ToolDefinition`、`ToolCall`、`ToolResult`：工具 schema、模型工具调用及工具返回均是一等协议类型，不使用无约束的 `serde_json::Value` 代替领域结构。
- `TokenUsage`、`StopReason`、`ModelCapabilities`。
- `ModelError`：结构化且不携带认证信息、完整 prompt、完整工具输出或未受限原始响应正文。
- `ModelStream`、`ModelStreamEvent`。

标准协议是厂商 API 与上层运行时之间的唯一边界。除 `peri-model` 内部 adapter 外，其他 crate 不得解析 provider-native SSE event 或 JSON payload。

### 流式优先的调用模型

`Model::stream` 是唯一的一等调用路径。`Model::complete` 必须通过聚合 `ModelStreamEvent` 实现，以保持流式与非流式请求、错误、取消和 usage 的统一语义。

概念 API：

```rust
#[async_trait]
pub trait Model: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities;

    fn prepare_request(
        &self,
        request: &ModelRequest,
    ) -> Result<PreparedModelRequest, ModelError>;

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError>;

    async fn complete(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse, ModelError>;
}
```

标准流事件至少包括：

- `TextDelta`；
- `ReasoningDelta`；
- `ToolCallDelta`，以稳定 index、可选 id/name 和 arguments 增量表达；
- `Usage`；
- `Completed(ModelResponse)`。

`ModelStream` 必须公开 `abort()`。它立即取消在途 HTTP/SSE 读取、取消 retry backoff 等待、丢弃 response body 并阻止后续重试。调用方提供的 `CancellationToken` 与 `abort()` 有相同的取消优先级；两者都返回 `ModelError::Cancelled`。

### Retry 与断连

retry 迁入 `peri-model`，由 `ModelRuntimeConfig::retry` 配置：最大尝试次数、初始/最大退避、jitter 和可重试 HTTP 状态或 transport error 分类。模型实例在创建时接收该运行时配置。

流式重试必须遵循以下不变量：

1. 仅在第一个可见 `ModelStreamEvent` 发出前自动重试。
2. 一旦已发出 `TextDelta`、`ReasoningDelta` 或 `ToolCallDelta`，后续 transport/协议失败必须返回 `ModelError::StreamInterrupted`，不得从头重放请求。
3. `abort()` 或外部取消到达后，必须立即停止在途连接和退避，不得再发起尝试，也不得将取消包装为可重试错误。
4. retry 可通过 protocol-neutral hook/observation 报告 attempt 与延迟；它不认识 `ExecutorEvent`、`AgentEventHandler` 或 Agent metrics。

### 请求准备与只读观测

`prepare_request` 与实际 `stream` 必须复用同一请求构造路径。`PreparedModelRequest` 为只读观测契约，允许 Langfuse 等上层记录真实的 provider-native request body，同时禁止泄漏认证信息。

公开内容仅限：

- protocol、model id、规范化 endpoint；
- 实际将发送的 provider-native JSON body；
- 安全的、显式定义的脱敏 metadata。

`Authorization`、API key、cookie、完整 headers 和内部 HTTP client 不得出现在 `PreparedModelRequest`、`ModelError`、日志或 retry observation 中。对可能包含用户敏感内容的观测 payload 实施长度限制和脱敏策略。

### 内建协议

首期只提供强类型、显式构造器：

```rust
OpenAiModel::new(OpenAiConfig)
AnthropicModel::new(AnthropicConfig)
```

`OpenAiConfig`、`AnthropicConfig` 和 `ModelRuntimeConfig` 由 `peri-model` 定义，且仅承载协议运行所需字段，例如 endpoint、model id、认证凭据载体、thinking、max tokens、stream 和 retry 配置。含有认证信息的配置不得实现会暴露 secret 的 `Debug` 或错误格式化。

新增协议须在 `peri-model` 中新增强类型构造器与协议契约测试；首期不提供 registry。

## 上层 crate 的职责

### `peri-agent`

保留 Agent runtime 语义：

- `ReactLLM`、`Reasoning`；
- `Model` 到 `ReactLLM` 的桥接；
- `peri-model` 标准消息/工具类型与 Agent 消息/工具类型的显式转换；
- `ModelStreamEvent` 到 `ExecutorEvent` 的映射；
- compact、TokenTracker 和 provider capability 到 Agent projection 的映射。

删除当前 `peri-agent::llm` 中的 provider transport、厂商 adapter、SSE parser、模型 DTO、模型 trait 和 Agent 级 retry decorator。Agent 不再拥有 retry 策略，只消费 `peri-model` 的 retry observation。

### `peri-acp`

保留应用级模型管理：

- `AppConfig`、配置文件 store 和 workspace/user config merge；
- 环境变量解析、provider type 解释、模型 alias 和用户配置校验；
- 将 ACP provider 配置转换为 `peri-model` 的强类型 config 并构造模型；
- `SessionContext`、`AgentPool`、模型实例生命周期和 cache invalidation；
- ACP event 映射与 Langfuse bridge。

`AgentPool`、辅助模型和 builder 改为持有 `peri_model::Model`。`TokenUsage`、`StopReason` 到 `peri-acp-types` DTO 的映射继续在 ACP 边界维护，不能让 DTO crate 依赖 runtime 类型。

### `peri-middlewares` 与 `peri-tui`

HITL、Goal 等直接模型调用方改为使用 `peri_model::Model` 及标准请求/响应类型。SubAgent 继续依赖 `peri-agent::ReactLLM`，仅在其需要底层辅助模型的位置依赖 `peri-model`。

TUI 继续经 ACP 管理 provider 配置和会话；不直接承载 provider 协议实现。

## 迁移与兼容性

这是一次性破坏性迁移：

1. 新建 workspace crate `peri-model` 并迁移标准协议、transport、OpenAI-compatible 和 Anthropic 实现及其测试。
2. 所有 workspace manifest 直接添加 `peri-model` 依赖，并将旧 `peri_agent::llm::*` 引用替换为新 API。
3. 在 `peri-agent` 实现显式 Agent bridge；保留 `ReactLLM`，删除 `BaseModelReactLLM`、`RetryableLLM` 和旧 LLM facade。
4. 删除 `peri-agent::llm` 模块及其 public re-export；不保留 type alias 或兼容入口。
5. 统一并消除现有多套消息 adapter 的协议语义分叉，最终由 `peri-model` adapter 成为 provider serialization/response parsing 的唯一事实源。

迁移不能改变以下可观察行为，除非另有独立需求批准：

- Anthropic system placement、cache control、extended thinking 和 token usage 语义；
- OpenAI-compatible tool schema、reasoning/thinking 兼容字段、session metadata 和 usage 语义；
- provider-native request payload 与实际 HTTP 请求 body 的一致性；
- ACP wire DTO 的序列化边界；
- 取消、工具调用和 Agent 事件的顺序语义。

## 验收与验证

### 协议 crate

- 每个 provider 保留或新增 request-body contract tests，覆盖 system、messages、tools、thinking、cache 和 session metadata。
- SSE 测试覆盖跨 chunk、UTF-8 边界、多行 data 与 `[DONE]`。
- 流式测试覆盖 text、reasoning、tool call 累积、usage 和完成响应。
- retry 测试覆盖可重试 transport/HTTP error、首事件前重试、首事件后中断、外部取消与 `abort()` 强制断连。
- 观测测试证明 `PreparedModelRequest` body 与实际请求同源，且不包含认证 headers/key。
- 错误测试证明错误、日志 fixture 和观测事件不泄漏 secret 或未受限请求内容。

### 跨 crate

- `peri-agent` 测试覆盖 `Model` 到 `ReactLLM`、`Reasoning` 和 `ExecutorEvent` 的桥接，以及 compact capability 映射。
- `peri-acp` 测试覆盖 provider config 转换、alias、AgentPool identity/invalidation、Langfuse usage 与 request payload 映射。
- `peri-middlewares` 测试覆盖 HITL 和 Goal 标准模型调用，以及 SubAgent runtime bridge。
- 全 workspace 搜索不得遗留 `peri_agent::llm`、`BaseModel`、`LlmRequest`、`LlmResponse`、`ChatOpenAI`、`ChatAnthropic` 的旧路径或兼容导出。

建议执行：

```bash
cargo check -p peri-model
cargo test -p peri-model --lib
cargo test -p peri-model --doc
cargo test -p peri-agent --lib
cargo test -p peri-acp --lib
cargo test -p peri-middlewares --lib
cargo test --workspace --doc
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```
