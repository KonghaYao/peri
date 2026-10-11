use super::projection::{
    MessageProjectionDirective, PersistedDirectiveRestore, ProjectionAction, ProjectionActionEntry,
    ProjectionTarget, ProviderCapabilities, ProviderProtocol, PROJECTION_POLICY_VERSION,
};
use crate::messages::{BaseMessage, MessageId};
use crate::session::transcript::MessageTranscript;
#[test]
fn test_projection_directive_serde_roundtrip() {
    let msg_id = MessageId::new();
    let directive = MessageProjectionDirective {
        policy_version: PROJECTION_POLICY_VERSION,
        entries: vec![ProjectionActionEntry {
            message_id: msg_id,
            target: ProjectionTarget::ToolCall {
                tool_call_id: "tc_123".into(),
            },
            action: ProjectionAction::CompactToolInput {
                fields: vec!["command".into(), "description".into()],
                keep_head: 600,
                keep_tail: 200,
            },
        }],
    };
    let json = serde_json::to_string(&directive).expect("序列化失败");
    let restored: MessageProjectionDirective = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(restored, directive);
    assert_eq!(restored.policy_version, PROJECTION_POLICY_VERSION);
    let ProjectionAction::CompactToolInput {
        fields,
        keep_head,
        keep_tail,
    } = &restored.entries[0].action
    else {
        panic!("应恢复 CompactToolInput action");
    };
    assert_eq!(fields, &["command", "description"]);
    assert_eq!(*keep_head, 600);
    assert_eq!(*keep_tail, 200);
}

#[test]
fn test_legacy_message_flags_deserialize_without_directive() {
    use peri_acp_types::store::MessageFlags;

    let legacy_json = r#"{"truncated":true,"excluded":false}"#;
    let flags: MessageFlags = serde_json::from_str(legacy_json).expect("旧 JSON 反序列化失败");
    assert!(flags.truncated);
    assert!(!flags.excluded);
    assert!(
        flags.projection.is_none(),
        "旧 JSON 反序列化后 projection 应为 None"
    );
}

#[test]
fn test_legacy_v1_compact_tool_input_deserializes_but_is_rejected_by_policy_version() {
    use peri_acp_types::store::MessageFlags;

    let mut transcript = MessageTranscript::new();
    let message = BaseMessage::human("legacy compacted content");
    let message_id = message.id();
    transcript.append(message);

    let legacy_v1_json = format!(
        r#"{{"truncated":true,"excluded":false,"projection":{{"policy_version":1,"entries":[{{"message_id":"{}","target":"Message","action":{{"CompactToolInput":{{"fields":["command"],"preserve_shape":true}}}}}}]}}}}"#,
        message_id.as_uuid()
    );
    let flags: MessageFlags =
        serde_json::from_str(&legacy_v1_json).expect("v1 CompactToolInput JSON 应可反序列化");
    let directive = flags
        .projection
        .expect("v1 JSON 应包含 projection directive");
    let ProjectionAction::CompactToolInput {
        fields,
        keep_head,
        keep_tail,
    } = &directive.entries[0].action
    else {
        panic!("应恢复 CompactToolInput action");
    };
    assert_eq!(fields, &["command"]);
    assert_eq!(*keep_head, 350);
    assert_eq!(*keep_tail, 100);

    transcript.set_flags_projection(message_id, directive);
    assert!(matches!(
        super::projection::plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION,),
        PersistedDirectiveRestore::Invalid
    ));
}

#[test]
fn test_message_flags_with_projection_serde_roundtrip() {
    use peri_acp_types::store::MessageFlags;

    let msg_id = MessageId::new();
    let flags = MessageFlags {
        truncated: true,
        excluded: false,
        projection: Some(MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: msg_id,
                target: ProjectionTarget::Message,
                action: ProjectionAction::Keep,
            }],
        }),
    };
    let json = serde_json::to_string(&flags).expect("序列化失败");
    let restored: MessageFlags = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(restored, flags);
}

#[test]
fn test_provider_capabilities_openai() {
    let caps = ProviderCapabilities::openai();
    assert_eq!(caps.protocol, ProviderProtocol::OpenAI);
    assert!(!caps.signed_reasoning_must_be_whole);
}

#[test]
fn test_provider_capabilities_anthropic() {
    let caps = ProviderCapabilities::anthropic();
    assert_eq!(caps.protocol, ProviderProtocol::Anthropic);
    assert!(caps.signed_reasoning_must_be_whole);
}

#[test]
fn test_provider_capabilities_default_safety() {
    let caps = ProviderCapabilities::default();
    assert_eq!(caps.protocol, ProviderProtocol::Generic);
    assert!(!caps.signed_reasoning_must_be_whole);
}

#[test]
fn test_projection_action_exclude_and_keep() {
    for action in [ProjectionAction::Exclude, ProjectionAction::Keep] {
        let entry = ProjectionActionEntry {
            message_id: MessageId::new(),
            target: ProjectionTarget::ContentBlock { index: 2 },
            action,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let restored: ProjectionActionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, entry);
    }
}
