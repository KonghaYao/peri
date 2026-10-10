//! 会话级外部系统指令的最终请求与 provider wire 回归。

use super::*;

#[test]
fn test_external_instructions_are_included_in_pressure_estimate() {
    let model = Arc::new(CaptureSystemModel {
        streamed_requests: Arc::new(Mutex::new(Vec::new())),
    });
    let plain = AgentModelBridge::from_arc(model.clone()).with_system("BASE");
    let extended = AgentModelBridge::from_arc(model)
        .with_system("BASE")
        .with_external_instructions(Some(Arc::from("x".repeat(4_000))));
    let messages = [BaseMessage::human("go")];
    assert!(
        extended.estimate_request_tokens(&messages, &[])
            >= plain.estimate_request_tokens(&messages, &[]) + 1_000
    );
}

/// [回归测试] ACP 扩展在 cache seam 后、逐请求贡献前进入真实 ModelRequest
/// 与两种 provider JSON；花括号、CRLF 和边缘空白保持字面值。
#[tokio::test]
async fn test_external_instructions_model_request_and_provider_wire() {
    let external = "  {{date}}\r\n外部规则  ";
    let with_internal_dynamic =
        format!("CACHED{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nUNCACHED_INTERNAL");
    for internal in ["BASE", "", with_internal_dynamic.as_str()] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bridge = AgentModelBridge::from_arc(Arc::new(CaptureSystemModel {
            streamed_requests: Arc::clone(&requests),
        }))
        .with_system(internal)
        .with_external_instructions(Some(Arc::from(external)))
        .with_system_contribution_provider(Arc::new(|| Ok("CONTRIBUTION".into())));
        bridge
            .generate_reasoning(&[BaseMessage::human("go")], &[], None)
            .await
            .unwrap();
        let request = requests.lock().unwrap().pop().unwrap();
        let system = request.messages[0].text_content().unwrap();
        assert_eq!(system.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 1);
        assert_eq!(system.matches(external).count(), 1);
        assert!(
            system.find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).unwrap() < system.find(external).unwrap()
        );
        assert!(system.find(external).unwrap() < system.find("CONTRIBUTION").unwrap());
        if internal.contains("UNCACHED_INTERNAL") {
            assert!(system.find("UNCACHED_INTERNAL").unwrap() < system.find(external).unwrap());
        }
        let anthropic = peri_model::anthropic::AnthropicModel::new(
            peri_model::anthropic::AnthropicConfig::new(
                url::Url::parse("https://example.invalid").unwrap(),
                "test",
                "claude-test",
            ),
        );
        let anth_body = anthropic
            .prepare_request(&request)
            .unwrap()
            .body()
            .as_value()
            .clone();
        let blocks = anth_body["system"].as_array().unwrap();
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block.get("cache_control").is_some())
                .count(),
            usize::from(!internal.is_empty())
        );
        let dynamic_text = blocks.last().unwrap()["text"].as_str().unwrap();
        assert_eq!(dynamic_text.matches(external).count(), 1);
        if internal.contains("UNCACHED_INTERNAL") {
            assert!(
                dynamic_text.find("UNCACHED_INTERNAL").unwrap()
                    < dynamic_text.find(external).unwrap()
            );
        }
        assert!(dynamic_text.contains(&format!(
            "<agent_instructions>\n{external}\n</agent_instructions>"
        )));
        let openai = peri_model::openai_compatible::OpenAiModel::new(
            peri_model::openai_compatible::OpenAiConfig::new(
                url::Url::parse("https://example.invalid").unwrap(),
                "test",
                "gpt-test",
            ),
        );
        let openai_body = openai
            .prepare_request(&request)
            .unwrap()
            .body()
            .as_value()
            .clone();
        let wire = openai_body["messages"][0]["content"].as_str().unwrap();
        assert_eq!(wire.matches(external).count(), 1);
        if internal.contains("UNCACHED_INTERNAL") {
            assert!(wire.find("UNCACHED_INTERNAL").unwrap() < wire.find(external).unwrap());
        }
        assert!(!wire.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    }
}

#[tokio::test]
async fn test_v1_embedded_external_instructions_fail_before_provider_when_unsafe() {
    for (prompt, derives) in [
        ("BASE<agent_instructions>legacy</agent_instructions>", true),
        (
            "BASE<agent_instructions>legacy__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__</agent_instructions>",
            false,
        ),
    ] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bridge = AgentModelBridge::from_arc(Arc::new(CaptureSystemModel {
            streamed_requests: Arc::clone(&requests),
        }))
        .with_system(prompt)
        .with_legacy_prompt_provenance(true, derives);
        assert!(bridge
            .generate_reasoning(&[BaseMessage::human("go")], &[], None)
            .await
            .is_err());
        assert!(requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn test_v1_embedded_external_instructions_main_prompt_keeps_original_bytes() {
    let prompt = "BASE\n<agent_instructions>\n  old bytes  \n</agent_instructions>";
    let requests = Arc::new(Mutex::new(Vec::new()));
    let bridge = AgentModelBridge::from_arc(Arc::new(CaptureSystemModel {
        streamed_requests: Arc::clone(&requests),
    }))
    .with_system(prompt)
    .with_legacy_prompt_provenance(true, false);
    bridge
        .generate_reasoning(&[BaseMessage::human("go")], &[], None)
        .await
        .unwrap();
    assert_eq!(
        requests.lock().unwrap()[0].messages[0]
            .text_content()
            .as_deref(),
        Some(prompt)
    );
}

#[tokio::test]
async fn test_external_instructions_create_boundary_without_internal_or_contribution() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let bridge = AgentModelBridge::from_arc(Arc::new(CaptureSystemModel {
        streamed_requests: Arc::clone(&requests),
    }))
    .with_system("")
    .with_external_instructions(Some(Arc::from("  literal  ")));
    bridge
        .generate_reasoning(&[BaseMessage::human("go")], &[], None)
        .await
        .unwrap();
    let request = requests.lock().unwrap().pop().unwrap();
    let system = request.messages[0].text_content().unwrap();
    assert_eq!(system.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 1);
    assert!(system.starts_with(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    assert!(system.contains("<agent_instructions>\n  literal  \n</agent_instructions>"));
}
