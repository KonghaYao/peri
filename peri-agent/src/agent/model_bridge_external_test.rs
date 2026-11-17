//! 会话级外部系统指令的最终请求与 provider wire 回归。

use crate::agent::model_bridge::tests::CaptureSystemModel;
use crate::agent::model_bridge::AgentModelBridge;
use crate::agent::react::ReactLLM as _;
use crate::messages::BaseMessage;
use peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
use peri_model::Model as _;
use std::sync::Arc;
use std::sync::Mutex;

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
        .with_environment_prompt(Some(Arc::from(
            "Working directory: /frozen\nToday's date: 2026-10-21",
        )))
        .with_session_id("current-session")
        .with_system_contribution_provider(Arc::new(|| Ok("CONTRIBUTION".into())));
        bridge
            .generate_reasoning(&[BaseMessage::human("go")], &[], None)
            .await
            .expect("the generate reasoning fixture result is required for this assertion");
        let request = requests
            .lock()
            .expect("requests lock mutex must remain unpoisoned throughout the test")
            .pop()
            .expect("requests lock fixture must contain the expected element");
        let system = request.messages[0]
            .text_content()
            .expect("the text content fixture result is required for this assertion");
        assert!(system.ends_with("<env>\nWorking directory: /frozen\nToday's date: 2026-10-21\nSession ID: current-session\n</env>"));
        assert_eq!(system.matches("<env>").count(), 1);
        assert!(
            system
                .find("CONTRIBUTION")
                .expect("the find fixture result is required for this assertion")
                < system
                    .find("<env>")
                    .expect("the find fixture result is required for this assertion")
        );
        assert_eq!(system.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 1);
        assert_eq!(system.matches(external).count(), 1);
        assert!(
            system
                .find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)
                .expect("the find fixture result is required for this assertion")
                < system
                    .find(external)
                    .expect("the find fixture result is required for this assertion")
        );
        assert!(
            system
                .find(external)
                .expect("the find fixture result is required for this assertion")
                < system
                    .find("CONTRIBUTION")
                    .expect("the find fixture result is required for this assertion")
        );
        if internal.contains("UNCACHED_INTERNAL") {
            assert!(
                system
                    .find("UNCACHED_INTERNAL")
                    .expect("the find fixture result is required for this assertion")
                    < system
                        .find(external)
                        .expect("the find fixture result is required for this assertion")
            );
        }
        let anthropic = peri_model::anthropic::AnthropicModel::new(
            peri_model::anthropic::AnthropicConfig::new(
                url::Url::parse("https://example.invalid")
                    .expect("the parse fixture result is required for this assertion"),
                "test",
                "claude-test",
            ),
        );
        let anth_body = anthropic
            .prepare_request(&request)
            .expect("the prepare request fixture result is required for this assertion")
            .body()
            .as_value()
            .clone();
        let blocks = anth_body["system"]
            .as_array()
            .expect("the as array fixture result is required for this assertion");
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block.get("cache_control").is_some())
                .count(),
            usize::from(!internal.is_empty())
        );
        let dynamic_text = blocks
            .last()
            .expect("blocks last fixture must contain the expected element")["text"]
            .as_str()
            .expect("the as str fixture result is required for this assertion");
        assert!(dynamic_text.ends_with("</env>"));
        assert!(dynamic_text.contains("Session ID: current-session"));
        assert_eq!(dynamic_text.matches(external).count(), 1);
        if internal.contains("UNCACHED_INTERNAL") {
            assert!(
                dynamic_text
                    .find("UNCACHED_INTERNAL")
                    .expect("the find fixture result is required for this assertion")
                    < dynamic_text
                        .find(external)
                        .expect("the find fixture result is required for this assertion")
            );
        }
        assert!(dynamic_text.contains(&format!(
            "<agent_instructions>\n{external}\n</agent_instructions>"
        )));
        let openai = peri_model::openai_compatible::OpenAiModel::new(
            peri_model::openai_compatible::OpenAiConfig::new(
                url::Url::parse("https://example.invalid")
                    .expect("the parse fixture result is required for this assertion"),
                "test",
                "gpt-test",
            ),
        );
        let openai_body = openai
            .prepare_request(&request)
            .expect("the prepare request fixture result is required for this assertion")
            .body()
            .as_value()
            .clone();
        let wire = openai_body["messages"][0]["content"]
            .as_str()
            .expect("the as str fixture result is required for this assertion");
        assert!(wire.ends_with("</env>"));
        assert!(wire.contains("Session ID: current-session"));
        assert_eq!(wire.matches(external).count(), 1);
        if internal.contains("UNCACHED_INTERNAL") {
            assert!(
                wire.find("UNCACHED_INTERNAL")
                    .expect("the find fixture result is required for this assertion")
                    < wire
                        .find(external)
                        .expect("the find fixture result is required for this assertion")
            );
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
        assert!(requests
            .lock()
            .expect("requests lock mutex must remain unpoisoned throughout the test")
            .is_empty());
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
        .expect("the generate reasoning fixture result is required for this assertion");
    assert_eq!(
        requests
            .lock()
            .expect("requests lock mutex must remain unpoisoned throughout the test")[0]
            .messages[0]
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
        .expect("the generate reasoning fixture result is required for this assertion");
    let request = requests
        .lock()
        .expect("requests lock mutex must remain unpoisoned throughout the test")
        .pop()
        .expect("requests lock fixture must contain the expected element");
    let system = request.messages[0]
        .text_content()
        .expect("the text content fixture result is required for this assertion");
    assert_eq!(system.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 1);
    assert!(system.starts_with(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    assert!(system.contains("<agent_instructions>\n  literal  \n</agent_instructions>"));
}

#[test]
fn test_environment_keeps_shared_prefix_and_uses_request_session_id() {
    let bridge = |session: &str, body: &str| {
        AgentModelBridge::from_arc(Arc::new(CaptureSystemModel {
            streamed_requests: Arc::new(Mutex::new(Vec::new())),
        }))
        .with_system("SHARED_BASE")
        .with_external_instructions(Some(Arc::from("SHARED_EXTERNAL")))
        .with_system_contribution_provider(Arc::new(|| Ok("SHARED_CONTRIBUTION".into())))
        .with_environment_prompt(Some(Arc::from(body)))
        .with_session_id(session)
    };
    let first = bridge("session-a", "Working directory: /a")
        .build_request(&[], &[])
        .expect("the build request fixture result is required for this assertion");
    let second = bridge("session-b", "Working directory: /b")
        .build_request(&[], &[])
        .expect("the build request fixture result is required for this assertion");
    let first_text = first.messages[0]
        .text_content()
        .expect("the text content fixture result is required for this assertion");
    let second_text = second.messages[0]
        .text_content()
        .expect("the text content fixture result is required for this assertion");
    assert_eq!(
        first_text
            .split_once("<env>")
            .expect("the split once fixture result is required for this assertion")
            .0,
        second_text
            .split_once("<env>")
            .expect("the split once fixture result is required for this assertion")
            .0
    );
    assert!(first_text.contains(&format!(
        "Session ID: {}",
        first
            .session_id
            .expect("the session id fixture result is required for this assertion")
    )));
    assert!(second_text.contains(&format!(
        "Session ID: {}",
        second
            .session_id
            .expect("the session id fixture result is required for this assertion")
    )));
}

#[test]
fn test_environment_rejects_missing_session_id_and_reserved_boundary() {
    let bridge = || {
        AgentModelBridge::from_arc(Arc::new(CaptureSystemModel {
            streamed_requests: Arc::new(Mutex::new(Vec::new())),
        }))
    };
    assert!(bridge()
        .with_environment_prompt(Some(Arc::from("environment")))
        .build_request(&[], &[])
        .is_err());
    assert!(bridge()
        .with_session_id("valid-session")
        .with_environment_prompt(Some(Arc::from(SYSTEM_PROMPT_DYNAMIC_BOUNDARY)))
        .build_request(&[], &[])
        .is_err());
    assert!(bridge()
        .with_session_id("bad\n</env>")
        .with_environment_prompt(Some(Arc::from("environment")))
        .build_request(&[], &[])
        .is_err());
    assert!(bridge()
        .with_session_id("valid-session")
        .build_request(&[], &[])
        .expect("the build request fixture result is required for this assertion")
        .messages
        .is_empty());
}
