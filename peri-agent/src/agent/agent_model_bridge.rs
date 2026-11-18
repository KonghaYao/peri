use super::model_message_conversion;
use std::collections::BTreeMap;
use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;

use peri_model::ContentBlock as ModelContentBlock;

use peri_model::JsonObject;
use peri_model::Model;
use peri_model::ModelMessage;
use peri_model::ModelRequest;
use peri_model::ModelResponse;
use peri_model::ModelStreamEvent;

use peri_model::ToolDefinition as ModelToolDefinition;
use peri_model::ToolResult as ModelToolResult;

/// Reserved model transport seam exposed through the Agent boundary for ACP admission.
pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str =
    peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;

use crate::agent::compact_v2::projection::ProviderCapabilities;
use crate::agent::compact_v2::projection::ProviderProtocol;
use crate::agent::events_v2::ObserveEvent;
use crate::agent::events_v2::RenderEvent;
use crate::agent::react::ReactLLM;
use crate::agent::react::Reasoning;
use crate::agent::react::StreamingContext;
use crate::agent::react::ToolCall;
use crate::error::AgentError;
use crate::error::AgentResult;
use crate::messages::BaseMessage;
use crate::messages::ContentBlock;

use crate::messages::MessageContent;
use crate::messages::MessageId;

use crate::middleware::PromptContributionError;
use crate::tools::BaseTool;

/// 将标准 `peri_model::Model` 接入 ReAct 的 Agent 边界。
pub struct AgentModelBridge {
    model: Arc<dyn Model>,
    system: Option<String>,
    external_instructions: Option<Arc<str>>,
    environment_prompt: Option<Arc<str>>,
    legacy_embedded_instructions: bool,
    derives_system_prompt: bool,
    system_contribution_provider: Option<SystemContributionProvider>,
    session_id: Option<String>,
    call_context: Option<peri_model::ModelCallContext>,
    /// Historical child identity replaced by the base system in model requests.
    absorb_system_message: Option<crate::messages::MessageId>,
}

/// 构造模型请求时同步读取当前 middleware prompt contribution 的 provider。
///
/// 返回当前贡献（非空贡献已按 `\n\n` 连接）或带来源的准入错误——provider
/// 边界负责把错误显式上抛，不静默剥离非法内容（M1）。
pub(crate) type SystemContributionProvider =
    Arc<dyn Fn() -> Result<String, PromptContributionError> + Send + Sync>;

impl AgentModelBridge {
    #[must_use]
    pub fn new(model: Arc<dyn Model>) -> Self {
        Self {
            model,
            system: None,
            external_instructions: None,
            environment_prompt: None,
            legacy_embedded_instructions: false,
            derives_system_prompt: false,
            system_contribution_provider: None,
            session_id: None,
            call_context: None,
            absorb_system_message: None,
        }
    }

    #[must_use]
    pub fn from_arc(model: Arc<dyn Model>) -> Self {
        Self::new(model)
    }

    #[must_use]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Freeze-owned client extension; never stored in the transcript or rendered template.
    #[must_use]
    pub fn with_external_instructions(mut self, instructions: Option<Arc<str>>) -> Self {
        self.external_instructions = instructions;
        self
    }

    /// Frozen environment body; append it after external instructions and middleware contributions.
    #[must_use]
    pub fn with_environment_prompt(mut self, body: Option<Arc<str>>) -> Self {
        self.environment_prompt = body;
        self
    }

    fn environment_suffix(&self) -> AgentResult<Option<String>> {
        let Some(body) = self.environment_prompt.as_deref() else {
            return Ok(None);
        };
        let session_id = self
            .session_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                AgentError::LlmError("environment prompt requires the executing session ID".into())
            })?;
        if body.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            || session_id.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
            || session_id.contains(['\n', '\r', '<', '>'])
        {
            return Err(AgentError::LlmError(
                "invalid environment prompt or session ID".into(),
            ));
        }
        Ok(Some(format!(
            "<env>\n{body}\nSession ID: {session_id}\n</env>"
        )))
    }

    /// V1 snapshots with an embedded extension can only reuse their original main prompt.
    #[must_use]
    pub fn with_legacy_prompt_provenance(mut self, embedded: bool, derives: bool) -> Self {
        self.legacy_embedded_instructions = embedded;
        self.derives_system_prompt = derives;
        self
    }

    /// 为每个模型请求提供当前的动态 system prompt 后缀。
    ///
    /// provider 同步执行并返回 owned `String`；调用结束后不会有锁 guard
    /// 跨越模型 await。空字符串保持 base system prompt 字节不变。
    pub(crate) fn with_system_contribution_provider(
        mut self,
        provider: SystemContributionProvider,
    ) -> Self {
        self.system_contribution_provider = Some(provider);
        self
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Bind one auxiliary call without changing the shared provider configuration.
    #[must_use]
    pub fn with_call_context(mut self, context: peri_model::ModelCallContext) -> Self {
        self.call_context = Some(context);
        self
    }

    /// Absorb the exact persisted identity message when the child base system replaces it.
    pub(crate) fn with_absorbed_system_message(
        mut self,
        identity: Option<crate::messages::MessageId>,
    ) -> Self {
        self.absorb_system_message = identity;
        self
    }

    /// 测试与过渡期调用方使用的单条消息转换入口。
    #[cfg(test)]
    pub(crate) fn convert_message(message: &BaseMessage) -> AgentResult<ModelMessage> {
        Self::convert_messages(std::slice::from_ref(message)).map(|mut messages| messages.remove(0))
    }

    pub(crate) fn projection_capabilities(model: &dyn Model) -> ProviderCapabilities {
        // 协议身份来自 prepared request 观测投影；prepare_request 失败时保守回退 Generic。
        // Anthropic 带签名 reasoning 必须整体保留（compact 投影依赖此判定），不能对
        // 所有 provider 一律报告 Generic——否则 Anthropic 的 signed reasoning 会被
        // projection 当成可截断内容处理。
        let protocol = model
            .prepare_request(&ModelRequest::default())
            .map(|request| match request.protocol() {
                peri_model::ProviderProtocol::OpenAiCompatible => ProviderProtocol::OpenAI,
                peri_model::ProviderProtocol::OpenAiResponses => ProviderProtocol::OpenAiResponses,
                peri_model::ProviderProtocol::Anthropic => ProviderProtocol::Anthropic,
                peri_model::ProviderProtocol::Other { .. } => ProviderProtocol::Generic,
            })
            .unwrap_or(ProviderProtocol::Generic);
        ProviderCapabilities {
            signed_reasoning_must_be_whole: protocol == ProviderProtocol::Anthropic,
            protocol,
        }
    }

    pub(crate) fn convert_messages(messages: &[BaseMessage]) -> AgentResult<Vec<ModelMessage>> {
        Self::convert_messages_skipping(messages, None)
    }

    /// 转换消息；`skip` 为需要在投影中吸收（丢弃）的下标（M3 归一化，至多一条）。
    fn convert_messages_skipping(
        messages: &[BaseMessage],
        skip: Option<usize>,
    ) -> AgentResult<Vec<ModelMessage>> {
        let mut tool_names = BTreeMap::new();
        for message in messages {
            if message.message_content().has_provider_state() {
                if !matches!(message, BaseMessage::Ai { .. }) {
                    return Err(AgentError::LlmError(
                        "provider replay state is only valid in assistant messages".into(),
                    ));
                }
                if let MessageContent::Blocks(blocks) = message.message_content() {
                    if blocks.iter().any(|block| {
                        matches!(block, ContentBlock::ToolResult { content, .. }
                            if content.iter().any(ContentBlock::has_provider_state))
                    }) {
                        return Err(AgentError::LlmError(
                            "provider replay state cannot be nested in tool results".into(),
                        ));
                    }
                }
            }
            if let BaseMessage::Ai { tool_calls, .. } = message {
                for call in tool_calls {
                    tool_names.insert(call.id.clone(), call.name.clone());
                }
            }
        }

        messages
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != skip)
            .map(|(_, message)| match message {
                BaseMessage::System { content, .. } => Ok(ModelMessage::System {
                    content: model_message_conversion::convert_content(content)?,
                }),
                BaseMessage::Human { content, .. } => Ok(ModelMessage::User {
                    content: model_message_conversion::convert_content(content)?,
                }),
                BaseMessage::Ai {
                    content,
                    tool_calls,
                    ..
                } => Ok(ModelMessage::Assistant {
                    content: model_message_conversion::convert_content(content)?,
                    tool_calls: tool_calls
                        .iter()
                        .map(model_message_conversion::convert_tool_call)
                        .collect::<AgentResult<_>>()?,
                }),
                BaseMessage::Tool {
                    tool_call_id,
                    content,
                    is_error,
                    ..
                } => {
                    let name = tool_names.get(tool_call_id).ok_or_else(|| {
                        AgentError::LlmError("tool result has no matching tool name".into())
                    })?;
                    Ok(ModelMessage::ToolResult {
                        result: ModelToolResult {
                            id: None,
                            tool_call_id: tool_call_id.clone(),
                            name: name.clone(),
                            content: model_message_conversion::convert_content(content)?,
                            is_error: *is_error,
                        },
                    })
                }
            })
            .collect()
    }

    fn build_request(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> AgentResult<ModelRequest> {
        if self.legacy_embedded_instructions {
            if self.derives_system_prompt {
                return Err(AgentError::LlmError("V1 frozen prompt contains embedded external instructions; create a new session before deriving an agent prompt".into()));
            }
            if self.system.as_deref().is_some_and(|prompt| {
                prompt
                    .split_once("<agent_instructions>")
                    .is_some_and(|(_, tail)| tail.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY))
            }) {
                return Err(AgentError::LlmError("V1 frozen external instructions contain a reserved cache boundary; create a new session".into()));
            }
        }
        let absorbed = self.absorb_system_message.and_then(|identity| {
            messages.iter().position(|message| {
                matches!(message, BaseMessage::System { .. }) && message.id() == identity
            })
        });
        let mut messages = Self::convert_messages_skipping(messages, absorbed)?;
        let dynamic = match self.system_contribution_provider.as_ref() {
            Some(provider) => Some(provider().map_err(|error| AgentError::MiddlewareError {
                middleware: error.middleware().to_string(),
                reason: error.to_string(),
            })?),
            None => None,
        };
        let base = match self.external_instructions.as_deref() {
            Some(external) => {
                let internal = self.system.as_deref().unwrap_or_default();
                let prefix = if internal.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count() == 1 {
                    internal.to_owned()
                } else {
                    format!("{internal}{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}")
                };
                Some(format!(
                    "{prefix}\n\n<agent_instructions>\n{external}\n</agent_instructions>"
                ))
            }
            None => self.system.clone(),
        };
        let system = match dynamic.as_deref() {
            Some(dynamic) => peri_model::prompt_cache::combine_system_prompt_with_dynamic(
                base.as_deref(),
                dynamic,
            ),
            None => base,
        };
        let system = match self.environment_suffix()? {
            Some(environment) => peri_model::prompt_cache::combine_system_prompt_with_dynamic(
                system.as_deref(),
                &environment,
            ),
            None => system,
        };
        if let Some(system) = system {
            messages.insert(0, ModelMessage::system_text(system));
        }
        let tools = tools
            .iter()
            .map(|tool| {
                Ok(ModelToolDefinition::new(
                    tool.name(),
                    JsonObject::from_value(tool.parameters())
                        .map_err(model_message_conversion::map_model_error)?,
                )
                .with_description(tool.description()))
            })
            .collect::<AgentResult<_>>()?;
        Ok(ModelRequest {
            messages,
            tools,
            session_id: self.session_id.clone(),
            call_context: self.call_context.clone(),
            ..Default::default()
        })
    }

    fn response_reasoning(response: ModelResponse, streamed: bool) -> AgentResult<Reasoning> {
        let usage = response.usage().cloned();
        let request_id = response.request_id().map(str::to_owned);
        let stop_reason = response.stop_reason().clone();
        let source_message = model_message_conversion::convert_model_message(response.message())?;
        let (content, tool_calls) = match response.message() {
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => (content, tool_calls),
            _ => {
                return Err(AgentError::LlmError(
                    "model response must be assistant".into(),
                ));
            }
        };
        let thought: String = content
            .iter()
            .filter_map(|block| match block {
                ModelContentBlock::Reasoning { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let answer = content
            .iter()
            .filter_map(ModelContentBlock::text_content)
            .collect::<String>();
        let tool_calls = tool_calls
            .iter()
            .map(|call| {
                Ok(ToolCall::new(
                    call.id(),
                    call.name(),
                    serde_json::Value::Object(
                        call.arguments().as_map().clone().into_iter().collect(),
                    ),
                ))
            })
            .collect::<AgentResult<Vec<_>>>()?;
        let has_answer = !answer.is_empty();
        let mut reasoning = if tool_calls.is_empty() {
            Reasoning::with_answer(thought, answer.clone())
        } else {
            Reasoning::with_tools(thought, tool_calls)
        };
        // Assistant 文本和 tool calls 可以在同一条完成消息中共存；不能因 ReAct
        // 当前要进入 Act 阶段就静默丢弃已生成文本。
        if has_answer {
            reasoning.final_answer = Some(answer);
        }
        reasoning.source_message = Some(source_message);
        reasoning.usage = usage;
        reasoning.request_id = request_id;
        reasoning.stop_reason = stop_reason;
        reasoning.streamed = streamed;
        Ok(reasoning)
    }

    /// 消费已构建的 request，驱动 Model 流式响应直至完成。
    ///
    /// 从 `generate_reasoning` 提取（2026-08 重构）：让观测路径
    /// （`generate_reasoning_with_observed_body`）与普通路径共享同一份
    /// 已构建的 request，消除每轮 LLM 调用的 request 双构建。
    async fn generate_from_request(
        &self,
        mut request: ModelRequest,
        streaming: Option<StreamingContext>,
        prepared: Option<peri_model::PreparedModelCall>,
    ) -> AgentResult<Reasoning> {
        if let Some(context) = streaming
            .as_ref()
            .and_then(|context| context.call_context.clone())
        {
            request.call_context = Some(context);
        }
        let model_name = self.model_name();
        // 本条 AI 消息的稳定身份：一次 LLM 调用 = 一条 assistant 消息。流式
        // chunk（ThinkingChunk/TextChunk）共享该 ID，流式结束构建 source_message
        // 时对齐同一 ID（ACP 标准 messageId 语义——chunk 的 messageId 即规范
        // 消息 ID，客户端据此判断消息边界/推理结束）。
        let message_id = MessageId::new();
        let cancellation = streaming
            .as_ref()
            .map(|context| context.cancel.clone())
            .unwrap_or_default();
        let mut stream = match prepared {
            Some(prepared) => prepared
                .start(cancellation.clone())
                .map_err(model_message_conversion::map_model_error)?,
            None => self
                .model
                .stream(request, cancellation.clone())
                .await
                .map_err(model_message_conversion::map_model_error)?,
        };

        // [Fix think-end] 本消息内是否已提前 emit ToolStarted：工具块开始
        // （Anthropic `content_block_start`，即 thinking 结束）时首个带
        // id/name 的 ToolCallDelta 到达，提前发 ToolStarted 让 TUI 立即冻结
        // 推理动画——工具参数流式生成期间 TUI 收不到任何事件（无正文时
        // "文本到达"推断永不触发），只能空转到 dispatch_tools 的正式
        // ToolStarted（模型流结束后才发出）。参数尚未生成 → input 置 Null，
        // 由 dispatch 路径同 id 的正式 ToolStarted 经 TUI start_tool 的
        // 重复 id upsert 填充。记录身份而非 bool：中断时须为已发出的
        // ToolStarted 补配对 ToolEnded（同 dispatch 路径的补发模式），否则
        // 续跑不回头重放该块，TUI 会永久停在"进行中"。
        let mut active_tool: Option<(String, String)> = None;
        let mut partial_text = String::new();

        loop {
            let event = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    stream.abort();
                    return Err(AgentError::Interrupted);
                }
                event = stream.next() => event,
            };
            match event {
                Some(Ok(ModelStreamEvent::TextDelta { text })) => {
                    partial_text.push_str(&text);
                    if let Some(context) = &streaming {
                        if context.cancel.is_cancelled() {
                            stream.abort();
                            return Err(AgentError::Interrupted);
                        }
                        // v2 直发（v1 ExecutorEvent 流式中间态已退役）：TextChunk 是
                        // 渲染层事件，身份（turn_id/agent_id）来自 StreamingContext。
                        context.event_bus.emit_render(RenderEvent::TextChunk {
                            turn_id: context.turn_id,
                            agent_id: context.agent_id,
                            message_id,
                            chunk: text,
                        });
                    }
                }
                Some(Ok(ModelStreamEvent::ReasoningDelta { text })) => {
                    if let Some(context) = &streaming {
                        if context.cancel.is_cancelled() {
                            stream.abort();
                            return Err(AgentError::Interrupted);
                        }
                        // v2 直发：ThinkingChunk（渲染层）+ AiReasoningChunk（观测层，
                        // Langfuse/tracer 消费）。主 agent 无 source_agent_id（None）；
                        // subagent 场景的 source_agent_id 由 forwarder 注入。
                        context.event_bus.emit_render(RenderEvent::ThinkingChunk {
                            turn_id: context.turn_id,
                            agent_id: context.agent_id,
                            message_id,
                            chunk: text.clone(),
                        });
                        context
                            .event_bus
                            .emit_observe(ObserveEvent::AiReasoningChunk {
                                turn_id: context.turn_id,
                                agent_id: context.agent_id,
                                text,
                                source_agent_id: None,
                            });
                    }
                }
                Some(Ok(ModelStreamEvent::ToolCallDelta { id, name, .. })) => {
                    // [Fix think-end] 工具块开始 = 推理结束：首个带 id/name 的
                    // delta 提前 emit ToolStarted（input 尚未生成 → Null）。
                    // 多工具并行时仅首个 delta 发射，其余工具由 dispatch 正式发。
                    if active_tool.is_none() {
                        if let Some(context) = &streaming {
                            if let (Some(id), Some(name)) = (id, name) {
                                if context.cancel.is_cancelled() {
                                    stream.abort();
                                    return Err(AgentError::Interrupted);
                                }
                                active_tool = Some((id.clone(), name.clone()));
                                context.event_bus.emit_render(RenderEvent::ToolStarted {
                                    turn_id: context.turn_id,
                                    agent_id: context.agent_id,
                                    tool_call_id: id.clone(),
                                    name: name.clone(),
                                    input: serde_json::Value::Null,
                                    // 本次 LLM 调用的稳定消息身份：纯工具响应轮次
                                    // 没有文本/推理 chunk，TUI 只能靠该字段归属。
                                    origin_message_id: Some(message_id),
                                    origin_readable: Some(true),
                                });
                            }
                        }
                    }
                    // 工具参数流式细节 TUI 不消费（与 Usage 一致丢弃）。
                }
                Some(Ok(ModelStreamEvent::Usage(_))) => {}
                Some(Ok(ModelStreamEvent::Completed(response))) => {
                    let mut reasoning = Self::response_reasoning(response, streaming.is_some())?;
                    // 安全截断可能丢弃半截工具；只关闭未交给 dispatch 的提前指示器。
                    if let (Some(context), Some((tool_call_id, name))) = (&streaming, &active_tool)
                    {
                        if !reasoning
                            .tool_calls
                            .iter()
                            .any(|call| &call.id == tool_call_id)
                        {
                            context.event_bus.emit_render(RenderEvent::ToolEnded {
                                turn_id: context.turn_id,
                                agent_id: context.agent_id,
                                tool_call_id: tool_call_id.clone(),
                                name: name.clone(),
                                output: "模型响应未完成该工具调用，已丢弃".to_string(),
                                is_error: true,
                                subagent_failure: None,
                            });
                        }
                    }
                    // 流式 chunk 的 messageId 与定型消息 ID 对齐（标准语义）。
                    if let Some(msg) = reasoning.source_message.take() {
                        reasoning.source_message = Some(msg.with_message_id(message_id));
                    }
                    reasoning.model = model_name;
                    return Ok(reasoning);
                }
                Some(Ok(ModelStreamEvent::Interrupted {
                    error,
                    attempts,
                    max_attempts,
                })) => {
                    if !error.interruption_logged() {
                        let diagnostic = error
                            .interruption_diagnostic()
                            .cloned()
                            .unwrap_or_else(|| error.diagnostic());
                        crate::session::retry_events::log_interruption(
                            &diagnostic,
                            attempts,
                            max_attempts,
                            self.session_id.as_deref(),
                            streaming.as_ref().map(|context| context.turn_id),
                        );
                    }
                    // 正文为空（只收到思考或半截工具）时不制造 assistant 消息：空
                    // 消息不是规范历史，且会被 provider 拒绝（Anthropic 对空 text
                    // block 返回 400）。此时仅靠续跑提醒继续。
                    let content = MessageContent::text(partial_text.clone());
                    let has_text = !content.is_empty();
                    let mut reasoning = Reasoning::with_answer(partial_text.clone(), partial_text);
                    if has_text {
                        reasoning.source_message =
                            Some(BaseMessage::ai(content).with_message_id(message_id));
                    } else {
                        reasoning.final_answer = None;
                    }
                    reasoning.streamed = streaming.is_some();
                    reasoning.model = model_name;
                    reasoning.stream_interruption = Some(crate::agent::react::StreamInterruption {
                        error,
                        attempts,
                        max_attempts,
                    });
                    // 已提前 emit 的 ToolStarted 必须补配对终态：中断后走续跑，
                    // 不会重放该工具块，缺 ToolEnded 会让 TUI 永久停在"进行中"。
                    if let (Some(context), Some((tool_call_id, name))) = (&streaming, &active_tool)
                    {
                        context.event_bus.emit_render(RenderEvent::ToolEnded {
                            turn_id: context.turn_id,
                            agent_id: context.agent_id,
                            tool_call_id: tool_call_id.clone(),
                            name: name.clone(),
                            output: "stream interrupted".to_string(),
                            is_error: true,
                            subagent_failure: None,
                        });
                    }
                    return Ok(reasoning);
                }
                Some(Err(error)) => return Err(model_message_conversion::map_model_error(error)),
                None => {
                    return Err(AgentError::ModelError(peri_model::ModelError::protocol(
                        peri_model::ProtocolErrorKind::StreamEndedWithoutCompleted,
                    )));
                }
            }
        }
    }
}

#[async_trait]
impl ReactLLM for AgentModelBridge {
    fn set_call_context(&mut self, context: peri_model::ModelCallContext) -> AgentResult<()> {
        self.call_context = Some(context);
        Ok(())
    }

    fn prepare_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> AgentResult<peri_model::PreparedModelCall> {
        self.model
            .prepare_stream(self.build_request(messages, tools)?)
            .map_err(model_message_conversion::map_model_error)
    }

    async fn generate_prepared_reasoning(
        &self,
        prepared: peri_model::PreparedModelCall,
        streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        if let Some(context) = streaming
            .as_ref()
            .and_then(|value| value.call_context.as_ref())
        {
            if self.call_context.as_ref() != Some(context) {
                return Err(AgentError::LlmError(
                    "Prepared call identity must be bound before preparation".into(),
                ));
            }
        }
        self.generate_from_request(ModelRequest::default(), streaming, Some(prepared))
            .await
    }
    fn estimate_request_tokens(&self, messages: &[BaseMessage], tools: &[&dyn BaseTool]) -> u64 {
        // 只读冻结前缀，不构建 provider 请求，也不第二次调用动态贡献 provider。
        // 冷启动尚未知动态后缀成本；后续有效 usage 会将其纳入权威基线。
        crate::agent::token::estimate_request_tokens(messages, tools)
            .saturating_add(
                self.environment_prompt
                    .as_ref()
                    .map(|body| {
                        ((body.chars().count()
                            + self
                                .session_id
                                .as_deref()
                                .unwrap_or_default()
                                .chars()
                                .count()
                            + "\n\n<env>\nSession ID: \n\n</env>".len())
                            as u64)
                            .div_ceil(4)
                    })
                    .unwrap_or(0),
            )
            .saturating_add(
                self.system
                    .as_ref()
                    .map(|system| (system.chars().count() as u64).div_ceil(4))
                    .unwrap_or(0),
            )
            .saturating_add(
                self.external_instructions
                    .as_ref()
                    .map(|external| {
                        // The fixed wrapper and cache seam are request-owned text too.
                        ((external.chars().count()
                            + "\n\n<agent_instructions>\n\n</agent_instructions>"
                                .chars()
                                .count()
                            + SYSTEM_PROMPT_DYNAMIC_BOUNDARY.len()) as u64)
                            .div_ceil(4)
                    })
                    .unwrap_or(0),
            )
    }

    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> AgentResult<Reasoning> {
        let request = self.build_request(messages, tools)?;
        self.generate_from_request(request, streaming, None).await
    }

    async fn generate_reasoning_with_observed_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        streaming: Option<StreamingContext>,
    ) -> AgentResult<(Reasoning, Option<serde_json::Value>)> {
        // 覆盖默认实现：只构建一次 request，观测体复用同一份（消除每轮双构建）
        let request = self.build_request(messages, tools)?;
        let observed_body = self
            .model
            .prepare_request(&request)
            .ok()
            .map(|prepared| prepared.body().as_value().clone());
        let reasoning = self.generate_from_request(request, streaming, None).await?;
        Ok((reasoning, observed_body))
    }

    fn model_name(&self) -> String {
        self.model
            .prepare_request(&ModelRequest::default())
            .map(|request| request.model_id().to_owned())
            .unwrap_or_else(|_| "unknown".into())
    }

    fn observed_provider_request_body(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> Option<serde_json::Value> {
        self.build_request(messages, tools)
            .ok()
            .and_then(|request| self.model.prepare_request(&request).ok())
            .map(|request| request.body().as_value().clone())
    }

    fn provider_capabilities(&self) -> ProviderCapabilities {
        Self::projection_capabilities(self.model.as_ref())
    }
}

#[cfg(test)]
use super::model_message_conversion::convert_model_message;

pub(crate) use super::model_message_conversion::map_model_error;

pub(crate) use super::model_message_conversion::stop_reason_display;

#[cfg(test)]
#[path = "model_bridge_test.rs"]
mod tests;

#[cfg(test)]
#[path = "model_bridge_replay_test.rs"]
mod replay_tests;

impl Debug for AgentModelBridge {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentModelBridge")
            .field("system_present", &self.system.is_some())
            .field(
                "external_instructions_present",
                &self.external_instructions.is_some(),
            )
            .field(
                "environment_prompt_present",
                &self.environment_prompt.is_some(),
            )
            .field(
                "legacy_embedded_instructions",
                &self.legacy_embedded_instructions,
            )
            .field("derives_system_prompt", &self.derives_system_prompt)
            .field(
                "system_contribution_provider_present",
                &self.system_contribution_provider.is_some(),
            )
            .field("session_id_present", &self.session_id.is_some())
            .field("call_context_present", &self.call_context.is_some())
            .field(
                "absorb_system_message",
                &self.absorb_system_message.is_some(),
            )
            .finish_non_exhaustive()
    }
}
