use serde_json::{json, Value};
use url::Url;

use crate::{
    ContentBlock, DocumentSource, ImageSource, JsonObject, MediaType, Model, ModelMessage,
    ModelRequest, ToolCall, ToolDefinition, ToolResult,
};

use super::{
    replay::state_block, request::build_request, response::project_items, ResponsesConfig,
    ResponsesModel, ResponsesProfile,
};

fn config(profile: ResponsesProfile) -> ResponsesConfig {
    ResponsesConfig::new(
        Url::parse("https://api.openai.com/v1").unwrap(),
        "test-secret",
        "test-model",
    )
    .with_profile(profile)
    .with_credential_ref("test-account")
    .with_max_tokens(999)
    .with_context_window(Some(128_000))
}

pub(super) fn tool() -> ToolDefinition {
    ToolDefinition::new(
        "read_file",
        JsonObject::from_value(
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        )
        .unwrap(),
    )
}

#[test]
fn chatgpt_profile_uses_namespaces_and_omits_unsupported_parameters() {
    let mut request = ModelRequest::new(vec![
        ModelMessage::system_text("system"),
        ModelMessage::user_text("question"),
    ])
    .with_tools(vec![tool()])
    .with_max_tokens(100);
    request.temperature = Some(0.6);
    request.session_id = Some("session".into());
    let config = config(ResponsesProfile::ChatGptPlan).with_reasoning_effort("high");
    let built = build_request(&config, &request).unwrap();
    assert_eq!(
        built.endpoint.as_str(),
        "https://api.openai.com/v1/responses"
    );
    assert_eq!(built.body["input"][0]["role"], "developer");
    assert_eq!(built.body["tools"][0]["type"], "namespace");
    assert_eq!(built.body["tools"][0]["name"], "peri");
    assert_eq!(built.body["tools"][0]["tools"][0]["name"], "read_file");
    assert_eq!(built.body["tools"][0]["tools"][0]["strict"], false);
    assert_eq!(
        built.body["reasoning"],
        json!({"effort":"high","summary":"auto"})
    );
    assert_eq!(built.body["store"], false);
    assert_eq!(built.body["stream"], true);
    for field in [
        "max_output_tokens",
        "temperature",
        "metadata",
        "previous_response_id",
        "conversation",
    ] {
        assert!(built.body.get(field).is_none(), "{field}");
    }
    let model = ResponsesModel::new(config);
    assert_eq!(model.output_token_limit(), None);
    assert_eq!(model.context_window(), Some(128_000));
}

#[test]
fn user_only_chatgpt_input_is_valid_and_platform_keeps_its_controls() {
    let mut request = ModelRequest::new(vec![ModelMessage::user_text("hello")])
        .with_tools(vec![tool()])
        .with_max_tokens(42);
    request.temperature = Some(0.5);
    let plan = build_request(&config(ResponsesProfile::ChatGptPlan), &request).unwrap();
    assert_eq!(plan.body["input"].as_array().unwrap().len(), 1);
    let platform = build_request(&config(ResponsesProfile::Platform), &request).unwrap();
    assert_eq!(platform.body["max_output_tokens"], 42);
    assert_eq!(platform.body["temperature"], 0.5);
    assert_eq!(platform.body["tools"][0]["type"], "function");
    assert_eq!(platform.body["tools"][0]["strict"], false);
}

#[test]
fn current_max_reasoning_effort_is_supported_and_unknown_efforts_fail_locally() {
    let body = build_request(
        &config(ResponsesProfile::Platform).with_reasoning_effort("max"),
        &ModelRequest::default(),
    )
    .unwrap()
    .body;
    assert_eq!(body["reasoning"]["effort"], "max");
    assert!(build_request(
        &config(ResponsesProfile::Platform).with_reasoning_effort("ultra"),
        &ModelRequest::default()
    )
    .is_err());
}

#[test]
fn chatgpt_credentials_are_restricted_to_the_exact_official_endpoint() {
    for endpoint in [
        "http://api.openai.com/v1",
        "https://api.openai.com.evil.test/v1",
        "https://api.openai.com:8443/v1",
        "https://api.openai.com/v2",
        "https://user:pass@api.openai.com/v1",
        "https://api.openai.com/v1?destination=evil",
    ] {
        let config = ResponsesConfig::new(Url::parse(endpoint).unwrap(), "secret", "model")
            .with_profile(ResponsesProfile::ChatGptPlan);
        assert!(
            build_request(&config, &ModelRequest::default()).is_err(),
            "{endpoint}"
        );
    }
    for endpoint in [
        "https://api.openai.com/v1",
        "https://api.openai.com/v1/",
        "https://api.openai.com/v1/responses",
    ] {
        let config = ResponsesConfig::new(Url::parse(endpoint).unwrap(), "secret", "model")
            .with_profile(ResponsesProfile::ChatGptPlan);
        assert!(build_request(&config, &ModelRequest::default()).is_ok());
    }
}

#[test]
fn maps_images_documents_and_function_outputs_without_flattening() {
    let image = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: MediaType::new("image/png"),
            data: "aW1n".into(),
        },
    };
    let request = ModelRequest::new(vec![
        ModelMessage::User {
            content: vec![
                image.clone(),
                ContentBlock::Document {
                    source: DocumentSource::Base64 {
                        media_type: MediaType::new("application/pdf"),
                        data: "cGRm".into(),
                    },
                    title: Some("a.pdf".into()),
                },
            ],
        },
        ModelMessage::tool_result(ToolResult {
            id: None,
            tool_call_id: "call_a".into(),
            name: "read_file".into(),
            content: vec![image],
            is_error: true,
        }),
    ]);
    let input = &build_request(&config(ResponsesProfile::Platform), &request)
        .unwrap()
        .body["input"];
    assert_eq!(
        input[0]["content"][0]["image_url"],
        "data:image/png;base64,aW1n"
    );
    assert_eq!(input[0]["content"][1]["filename"], "a.pdf");
    assert_eq!(input[1]["type"], "function_call_output");
    assert_eq!(input[1]["call_id"], "call_a");
    assert_eq!(input[1]["output"][1]["type"], "input_image");
    assert!(input[1]["output"][0]["text"]
        .as_str()
        .unwrap()
        .contains("is_error=true"));
}

fn state_message(config: &ResponsesConfig, items: &[Value]) -> ModelMessage {
    let binding = build_request(config, &ModelRequest::default())
        .unwrap()
        .binding;
    let (mut content, calls) = project_items(items, config.profile, None).unwrap();
    content.push(state_block(&binding, items));
    ModelMessage::assistant(content, calls)
}

pub(super) fn output_items() -> Vec<Value> {
    vec![
        json!({"type":"message","id":"msg_a","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"Checking","annotations":[]}]}),
        json!({"type":"reasoning","id":"rs_a","summary":[{"type":"summary_text","text":"Need file"}],"encrypted_content":"opaque-ciphertext"}),
        json!({"type":"function_call","id":"fc_a","call_id":"call_a","name":"read_file","arguments":"{\"path\":\"a.txt\"}","status":"completed"}),
    ]
}

#[test]
fn native_replay_preserves_order_ids_phase_encrypted_state_and_avoids_duplicates() {
    let config = config(ResponsesProfile::Platform);
    let items = output_items();
    let request = ModelRequest::new(vec![
        state_message(&config, &items),
        ModelMessage::tool_result(ToolResult::success("call_a", "read_file", "contents")),
    ]);
    let built = build_request(&config, &request).unwrap();
    assert_eq!(&built.body["input"].as_array().unwrap()[..3], &items);
    assert_eq!(built.body["input"].as_array().unwrap().len(), 4);
    assert_eq!(built.body["input"][3]["call_id"], "call_a");
    assert_ne!(
        built.body["input"][2]["id"],
        built.body["input"][2]["call_id"]
    );
}

#[test]
fn replay_requires_matching_projection_and_does_not_cross_credential_domain() {
    let config = config(ResponsesProfile::Platform);
    let original = state_message(&config, &output_items());
    let foreign = super::ResponsesConfig::new(
        Url::parse("https://api.openai.com/v1").unwrap(),
        "other-secret",
        "test-model",
    )
    .with_credential_ref("other-account");
    let body = build_request(&foreign, &ModelRequest::new(vec![original.clone()]))
        .unwrap()
        .body;
    assert!(!body.to_string().contains("opaque-ciphertext"));
    assert!(!body.to_string().contains("fc_a"));
    assert_eq!(body["input"][1]["call_id"], "call_a");
    let mut changed = original;
    if let ModelMessage::Assistant { content, .. } = &mut changed {
        content[0] = ContentBlock::text("different projection");
    }
    assert!(build_request(&config, &ModelRequest::new(vec![changed])).is_err());
}

#[test]
fn opaque_state_is_rejected_outside_assistant_and_tool_arguments_are_objects() {
    let state = ContentBlock::ProviderState {
        provider: "openai_responses".into(),
        data: json!({}),
    };
    for message in [
        ModelMessage::User {
            content: vec![state.clone()],
        },
        ModelMessage::System {
            content: vec![state.clone()],
        },
        ModelMessage::tool_result(ToolResult {
            id: None,
            tool_call_id: "call_a".into(),
            name: "read_file".into(),
            content: vec![state],
            is_error: false,
        }),
    ] {
        assert!(build_request(
            &config(ResponsesProfile::Platform),
            &ModelRequest::new(vec![message])
        )
        .is_err());
    }
    let mut items = output_items();
    items[2]["arguments"] = json!("[]");
    assert!(project_items(&items, ResponsesProfile::Platform, None).is_err());
}

#[test]
fn normalized_calls_use_call_id_and_plan_namespace() {
    let call = ToolCall::new(
        "call_a",
        "read_file",
        JsonObject::from_value(json!({"path":"a.txt"})).unwrap(),
    );
    let request = ModelRequest::new(vec![ModelMessage::assistant(Vec::new(), vec![call])]);
    let body = build_request(&config(ResponsesProfile::ChatGptPlan), &request)
        .unwrap()
        .body;
    assert_eq!(body["input"][0]["namespace"], "peri");
    assert_eq!(body["input"][0]["call_id"], "call_a");
    assert!(body["input"][0].get("id").is_none());
}

#[test]
fn cross_endpoint_model_and_profile_replay_keeps_only_the_typed_projection() {
    let config = config(ResponsesProfile::Platform);
    let message = state_message(&config, &output_items());
    let alternatives = [
        ResponsesConfig::new(
            Url::parse("https://proxy.example/v1").unwrap(),
            "secret",
            "test-model",
        )
        .with_credential_ref("test-account"),
        ResponsesConfig::new(
            Url::parse("https://api.openai.com/v1").unwrap(),
            "secret",
            "other-model",
        )
        .with_credential_ref("test-account"),
        super::request_test::config(ResponsesProfile::ChatGptPlan),
    ];
    for alternate in alternatives {
        let body = build_request(&alternate, &ModelRequest::new(vec![message.clone()]))
            .unwrap()
            .body;
        assert!(!body.to_string().contains("opaque-ciphertext"));
        assert!(!body.to_string().contains("fc_a"));
        assert_eq!(body["input"][1]["call_id"], "call_a");
    }
}

#[test]
fn invalid_or_duplicate_function_names_fail_before_transport() {
    for name in [
        String::new(),
        "a".repeat(65),
        "mcp.tool".into(),
        "非ASCII".into(),
    ] {
        let mut tool = tool();
        tool.name = name;
        assert!(build_request(
            &config(ResponsesProfile::Platform),
            &ModelRequest::default().with_tools(vec![tool])
        )
        .is_err());
    }
    assert!(build_request(
        &config(ResponsesProfile::Platform),
        &ModelRequest::default().with_tools(vec![tool(), tool()])
    )
    .is_err());
}

#[test]
fn replay_rejects_duplicate_native_item_ids_and_call_ids() {
    let mut items = output_items();
    items.push(items[2].clone());
    assert!(project_items(&items, ResponsesProfile::Platform, None).is_err());
    items[3]["id"] = json!("fc_new");
    assert!(project_items(&items, ResponsesProfile::Platform, None).is_err());
}
