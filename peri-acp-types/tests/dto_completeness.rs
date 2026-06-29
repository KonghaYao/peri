//! Assert every DTO type needed by TUI exists and round-trips through serde.
use peri_acp_types::*;
use serde_json::json;

#[test]
fn test_base_message_dto_roundtrip() {
    let msg = message::BaseMessageDto::human("hello");
    let j = serde_json::to_value(&msg).unwrap();
    let back: message::BaseMessageDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn test_content_block_dto_all_variants() {
    let blocks = vec![
        message::ContentBlockDto::text("hi"),
        message::ContentBlockDto::tool_use("t1", "Bash", json!({"cmd": "ls"})),
        message::ContentBlockDto::tool_result("t1", "output", false),
        message::ContentBlockDto::reasoning("thinking...", Some("sig".into())),
    ];
    for b in blocks {
        let j = serde_json::to_value(&b).unwrap();
        let back: message::ContentBlockDto = serde_json::from_value(j).unwrap();
        assert_eq!(back, b);
    }
}

#[test]
fn test_interaction_context_dto() {
    let ctx = interaction::InteractionContextDto {
        session_id: "s1".into(),
        prompt_id: "p1".into(),
        channel_state: interaction::ChannelStateDto::Ready,
    };
    let j = serde_json::to_value(&ctx).unwrap();
    let back: interaction::InteractionContextDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, ctx);
}

#[test]
fn test_mcp_dto_set() {
    let _server = mcp_types::ServerInfoDto {
        name: "mcp1".into(),
        transport_type: "stdio".into(),
        status: mcp_types::ClientStatusDto::Connected,
        tool_count: 5,
        resource_count: 2,
        oauth_status: mcp_types::OAuthStatusDto::None,
        source: Some(mcp_types::ConfigSourceDto::Project {
            path: "/tmp".into(),
        }),
        url: None,
        plugin_source: None,
    };
    let _oauth = mcp_types::OAuthStatusDto::Authorized;
    let _init = mcp_types::McpInitStatusDto::Ready { total: 3 };
}

#[test]
fn test_hook_dto_set() {
    let _h = hook::RegisteredHookDto {
        id: "h1".into(),
        event: hook::HookEventDto::PreToolUse,
        hook_type: hook::HookTypeDto::Command { cmd: "echo".into() },
        enabled: true,
    };
}

#[test]
fn test_plugin_dto_set() {
    let _scope = plugin_types::InstallScopeDto::User;
    let _src = plugin_types::MarketplaceSourceDto::Git { url: "x".into() };
    let _cmd = plugin_types::CommandEntryDto {
        name: "c1".into(),
        source: plugin_types::CommandSourceDto::Builtin,
    };
}

#[test]
fn test_skill_metadata_dto() {
    let _s = skill::SkillMetadataDto {
        name: "writer".into(),
        description: "...".into(),
        path: "/fake/writer/SKILL.md".into(),
        source: skill::SkillSourceDto::Builtin,
        plugin_name: None,
        disabled: false,
    };
}

#[test]
fn test_permission_mode_dto() {
    for m in &[
        permission::PermissionModeDto::Default,
        permission::PermissionModeDto::AcceptEdits,
        permission::PermissionModeDto::Plan,
        permission::PermissionModeDto::Yolo,
    ] {
        let j = serde_json::to_value(m).unwrap();
        let back: permission::PermissionModeDto = serde_json::from_value(j).unwrap();
        assert_eq!(&back, m);
    }
}

#[test]
fn test_hitl_and_ask_user_dtos() {
    let _item = interaction_types::BatchItemDto {
        tool_name: "Bash".into(),
        input_summary: "ls".into(),
        tool_call_id: "tc1".into(),
    };
    let _decision = interaction_types::HitlDecisionDto::Accept;
    let _q = interaction_types::AskUserQuestionDataDto {
        question: "q?".into(),
        options: vec![],
    };
    let _thread = interaction_types::ThreadMetaDto {
        thread_id: "t1".into(),
        title: "test".into(),
        created_at: 1000,
        updated_at: 2000,
    };
}

#[test]
fn test_message_content_dto() {
    // Text variant
    let text = message::MessageContentDto::Text("hello".into());
    let j = serde_json::to_value(&text).unwrap();
    let back: message::MessageContentDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, text);

    // Blocks variant
    let blocks = message::MessageContentDto::Blocks(vec![
        message::ContentBlockDto::text("block1"),
        message::ContentBlockDto::tool_use("id1", "Bash", json!({"cmd": "ls"})),
    ]);
    let j = serde_json::to_value(&blocks).unwrap();
    let back: message::MessageContentDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, blocks);
}

#[test]
fn test_oauth_callback_result_dto() {
    let r = mcp_types::OAuthCallbackResultDto {
        code: "abc123".into(),
        state: "xyz789".into(),
    };
    let j = serde_json::to_value(&r).unwrap();
    let back: mcp_types::OAuthCallbackResultDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, r);
}

#[test]
fn test_request_record_dto() {
    let r = interaction_types::RequestRecordDto {
        request_id: Some("req1".into()),
        input_tokens: 100,
        output_tokens: 50,
        cache_read_tokens: Some(10),
        cache_creation_tokens: None,
        model: Some("claude-3".into()),
        timestamp: 1700000000,
    };
    let j = serde_json::to_value(&r).unwrap();
    let back: interaction_types::RequestRecordDto = serde_json::from_value(j).unwrap();
    assert_eq!(back, r);
}
