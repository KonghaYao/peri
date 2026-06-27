    struct MockBaseModel {
        id: &'static str,
        window: u32,
    }
    #[async_trait::async_trait]
    impl super::super::BaseModel for MockBaseModel {
        async fn invoke(
            &self,
            _: super::super::types::LlmRequest,
        ) -> crate::error::AgentResult<super::super::types::LlmResponse> {
            unimplemented!()
        }
        fn provider_name(&self) -> &str {
            "mock"
        }
        fn model_id(&self) -> &str {
            self.id
        }
        fn context_window(&self) -> u32 {
            self.window
        }
    }

    #[test]
    fn test_context_window_delegates_to_model() {
        let llm = BaseModelReactLLM::new(Box::new(MockBaseModel {
            id: "any-model",
            window: 128_000,
        }));
        assert_eq!(llm.context_window(), 128_000);
    }

    #[test]
    fn test_context_window_default_from_trait() {
        let llm = BaseModelReactLLM::new(Box::new(MockBaseModel {
            id: "unknown",
            window: 200_000,
        }));
        assert_eq!(llm.context_window(), 200_000);
    }

    /// 验证：当 stop_reason == EndTurn 但响应含 tool_use blocks 时，
    /// generate_reasoning 仍走工具调用路径（而非最终回答路径），
    /// 防止 source_message 中的 tool_use 成为孤儿导致 API 400。
    #[tokio::test]
    async fn test_stop_reason_mismatch_with_tool_use_blocks_treated_as_tool_call() {
        use super::*;
        use crate::llm::types::{LlmResponse, StopReason};
        use crate::messages::BaseMessage;

        // 模拟 DeepSeek 返回 stop_reason=end_turn 但内容含 tool_use
        struct DeepSeekStopReasonMock;
        #[async_trait::async_trait]
        impl super::super::BaseModel for DeepSeekStopReasonMock {
            async fn invoke(
                &self,
                _: super::super::types::LlmRequest,
            ) -> crate::error::AgentResult<super::super::types::LlmResponse> {
                let msg = BaseMessage::ai_with_tool_calls(
                    crate::messages::MessageContent::text("I'll write that file"),
                    vec![crate::messages::ToolCallRequest::new(
                        "call_00_abc".to_string(),
                        "Write".to_string(),
                        serde_json::json!({"file_path": "/tmp/test.txt", "content": "hello"}),
                    )],
                );
                Ok(LlmResponse {
                    message: msg,
                    stop_reason: StopReason::EndTurn, // 关键：stop_reason 不是 ToolUse
                    usage: None,
                    request_id: None,
                })
            }
            fn provider_name(&self) -> &str {
                "deepseek"
            }
            fn model_id(&self) -> &str {
                "deepseek-chat"
            }
            fn context_window(&self) -> u32 {
                128_000
            }
        }

        let llm = BaseModelReactLLM::new(Box::new(DeepSeekStopReasonMock));
        let tools: Vec<&dyn crate::tools::BaseTool> = vec![];
        let result = llm
            .generate_reasoning(&[], &tools, None)
            .await
            .expect("generate_reasoning 应成功");

        // 关键断言：即使 stop_reason 是 EndTurn，tool_use blocks 存在时应走工具调用路径
        assert!(
            result.needs_tool_call(),
            "stop_reason=EndTurn 但内容含 tool_use 时，应走工具调用路径，实际走了最终回答路径"
        );
        assert_eq!(result.tool_calls.len(), 1, "应提取到 1 个工具调用");
        assert_eq!(result.tool_calls[0].name, "Write");
        assert_eq!(result.tool_calls[0].id, "call_00_abc");
    }

    /// 验证：stop_reason == EndTurn 且内容不含 tool_use 时，正常走最终回答路径。
    #[tokio::test]
    async fn test_stop_reason_end_turn_without_tool_use_treated_as_answer() {
        use super::*;
        use crate::llm::types::{LlmResponse, StopReason};
        use crate::messages::BaseMessage;

        struct NormalEndTurnMock;
        #[async_trait::async_trait]
        impl super::super::BaseModel for NormalEndTurnMock {
            async fn invoke(
                &self,
                _: super::super::types::LlmRequest,
            ) -> crate::error::AgentResult<super::super::types::LlmResponse> {
                let msg = BaseMessage::ai("This is a normal response");
                Ok(LlmResponse {
                    message: msg,
                    stop_reason: StopReason::EndTurn,
                    usage: None,
                    request_id: None,
                })
            }
            fn provider_name(&self) -> &str {
                "mock"
            }
            fn model_id(&self) -> &str {
                "mock-model"
            }
            fn context_window(&self) -> u32 {
                128_000
            }
        }

        let llm = BaseModelReactLLM::new(Box::new(NormalEndTurnMock));
        let tools: Vec<&dyn crate::tools::BaseTool> = vec![];
        let result = llm
            .generate_reasoning(&[], &tools, None)
            .await
            .expect("generate_reasoning 应成功");

        assert!(
            !result.needs_tool_call(),
            "stop_reason=EndTurn 且无 tool_use 时，应走最终回答路径"
        );
        assert_eq!(result.final_answer.as_deref(), Some("This is a normal response"));
    }

    /// 验证：`build_provider_request_body` 应透传 BaseModel 返回的 raw body，
    /// 并包含 `self.system`（与实际 invoke 请求体同源）。
    ///
    /// Langfuse Generation input 上传的关键不变量：raw_body 必须与 Provider 实际
    /// 收到的请求体一致（含 system 字段），否则 UI 显示与实际行为分叉。
    /// 共享 `build_full_llm_request` helper（validate agent 风险点 #3）。
    #[test]
    fn test_build_provider_request_body_includes_system_and_delegates_to_base_model() {
        use super::*;
        use crate::llm::types::LlmRequest;

        struct RawBodyMock;
        #[async_trait::async_trait]
        impl super::super::BaseModel for RawBodyMock {
            async fn invoke(
                &self,
                _: LlmRequest,
            ) -> crate::error::AgentResult<super::super::types::LlmResponse> {
                unimplemented!()
            }
            fn provider_name(&self) -> &str {
                "mock"
            }
            fn model_id(&self) -> &str {
                "mock-model"
            }
            // 关键：override build_request_body 返回 Provider-native 完整 body
            fn build_request_body(
                &self,
                request: &LlmRequest,
            ) -> Option<serde_json::Value> {
                // 模拟 OpenAI-style：system 在 messages[0]，tools 是 function wrapper
                let messages = serde_json::json!({
                    "role": "system",
                    "content": request.system.clone().unwrap_or_default(),
                });
                Some(serde_json::json!({
                    "model": "mock-model",
                    "messages": [messages],
                    "tools": [{"type":"function","function":{"name":"X"}}],
                }))
            }
        }

        let llm = BaseModelReactLLM::new(Box::new(RawBodyMock))
            .with_system("SYSTEM_PROMPT_BODY".to_string());
        let body = llm
            .build_provider_request_body(&[], &[])
            .expect("BaseModel override 后应返回 Some(body)");
        let obj = body.as_object().expect("body 应是 object");
        assert_eq!(obj["model"], "mock-model");
        // system 透传：BaseModel 的 build_request_body 应看到 self.system
        let messages = obj["messages"].as_array().expect("messages 应是 array");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "SYSTEM_PROMPT_BODY");
    }

    /// 验证：BaseModel 未 override `build_request_body` 时，trait 默认 None 被透传，
    /// 上游 emit LlmRequestPayload 不会触发（reason.rs `if let Some(body) = ...` 守卫）。
    #[test]
    fn test_build_provider_request_body_returns_none_when_base_model_default() {
        let llm = BaseModelReactLLM::new(Box::new(MockBaseModel {
            id: "any-model",
            window: 128_000,
        }));
        assert!(
            llm.build_provider_request_body(&[], &[]).is_none(),
            "BaseModel 默认 None → ReactLLM 也应返回 None"
        );
    }
