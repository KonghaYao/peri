# P1-7：ProviderAdapter trait — 统一 Anthropic/OpenAI invoke 差异

**状态**：Open
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-16
**来源**：`spec/issues/2026-07-16-architecture-upgrade-checklist.md` P1-7

## Problem Statement

`peri-agent/src/llm/anthropic/invoke.rs` 和 `peri-agent/src/llm/openai/invoke.rs` 是两个平行的 Provider 实现。两者共享接近 70% 的结构（消息序列化、system prompt hoist、streaming 处理），但无共享抽象：

- 相同的 `build_system_prompt` 逻辑在两个文件中重复
- `content_block_to_api_format` 转换逻辑因 Anthropic/OpenAI 格式差异而平行实现
- streaming chunk 处理流程相似但 API 结构不同

当前通过 `BaseModelReactLLM` 的 `react()` 方法间接调用 `ChatAnthropic::invoke()` / `ChatOpenAI::invoke()`，但这只是薄封装，未提取真正的共享逻辑。

## 建议方案

提取 `ProviderAdapter` trait 封装 Provider 特定差异：

```rust
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// 将 BaseMessage 序列化为 Provider 特定 API 格式
    fn serialize_message(&self, msg: &BaseMessage) -> ProviderMessage;
    
    /// 构造 API 请求体（含 system prompt hoist + tool definitions + thinking config）
    fn build_request(&self, req: &LlmRequest) -> ProviderRequest;
    
    /// 解析 streaming chunk 为 tool_use_delta / text_delta / stop
    fn parse_chunk(&self, chunk: &ProviderChunk) -> ParsedChunk;
    
    /// 解析最终响应（非 streaming）
    fn parse_response(&self, response: &ProviderResponse) -> LlmResponse;
}

// AnthropicAdapter / OpenAiAdapter 各自实现此 trait
```

## 风险

- **中高**：LLM 调用路径是 Agent 核心热路径，修改需要充分测试
- Anthropic 的 streaming SSE 事件格式与 OpenAI 的 JSON streaming 差异较大，需要仔细设计 `parse_chunk` 返回类型
- `retry.rs` 的 `RetryableLLM` 包装需要适配新 trait

## 实施要点

1. 先定义 trait（`peri-agent/src/llm/adapter.rs`），不改现有代码
2. 为 `ChatAnthropic` 和 `ChatOpenAI` 分别实现 trait
3. 将 `BaseModelReactLLM` 改为泛型 `<A: ProviderAdapter>`，委托 adapter 处理 Provider 差异
4. 删除 `invoke.rs` 中的重复 `build_system_prompt` / `convert_messages` 逻辑

## 相关文件

- `peri-agent/src/llm/anthropic/invoke.rs` — Anthropic invoke 实现
- `peri-agent/src/llm/openai/invoke.rs` — OpenAI invoke 实现
- `peri-agent/src/llm/react_adapter.rs` — BaseModelReactLLM（调用入口）
- `peri-agent/src/llm/mod.rs` — LLM 模块入口
- `peri-agent/src/llm/types.rs` — LlmRequest / LlmResponse 类型定义
- `peri-agent/src/llm/retry.rs` — RetryableLLM 包装
