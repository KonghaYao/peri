use crate::identity::AttemptId;
use crate::interaction::AttachmentId;
use crate::interaction::ConnectionEpoch;
use crate::interaction::ElicitationDecision;
use crate::interaction::ElicitationRequest;
use crate::interaction::InteractionAttribution;
use crate::interaction::InteractionContext;
use crate::interaction::InteractionResponse;
use crate::interaction::TrustedInteractionOwner;
use crate::interaction::UnansweredCause;
use crate::interaction::ELICITATION_UNANSWERED_META_KEY;
use crate::session::ExecutionBinding;
use crate::session::TurnId;

fn binding() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

#[test]
fn model_owner_projects_real_turn_facts() {
    let binding = binding();
    let owner = TrustedInteractionOwner::model("session-1", binding.clone());

    assert_eq!(owner.kind(), "model");
    assert_eq!(owner.session_id(), Some("session-1"));
    let (session_id, turn_id, attempt_id) = owner.model_turn().expect("model owner has turn facts");
    assert_eq!(session_id, "session-1");
    assert_eq!(turn_id, &binding.turn_id);
    assert_eq!(attempt_id, &binding.attempt_id);
}

#[test]
fn model_owner_keeps_turn_and_attempt_distinct() {
    let first = TrustedInteractionOwner::model("s", binding());
    let second = TrustedInteractionOwner::model("s", binding());
    let (_, first_turn, first_attempt) = first.model_turn().expect("model");
    let (_, second_turn, second_attempt) = second.model_turn().expect("model");

    // 两个 turn 身份必须彼此独立：turn/attempt 都不得跨 turn 复用。
    assert_ne!(first_turn, second_turn);
    assert_ne!(first_attempt, second_attempt);
}

#[test]
fn app_owner_locates_session_by_attachment() {
    let owner = TrustedInteractionOwner::app("app-1", "att-1", ConnectionEpoch::initial());

    assert_eq!(owner.kind(), "app");
    assert_eq!(owner.session_id(), None);
    assert!(owner.model_turn().is_none());
    assert_eq!(
        owner.to_string(),
        "app(app=app-1, attachment=att-1, epoch=1)"
    );
}

#[test]
fn connection_epoch_only_moves_forward() {
    let first = ConnectionEpoch::initial();
    let second = first.next();
    assert_eq!(first.get(), 1);
    assert_eq!(second.get(), 2);
    assert!(second > first);
    // 旧连接纪元不等于当前纪元：重连后旧纪元不再匹配。
    assert_ne!(first, second);
}

#[test]
fn host_owner_reuses_invocation_origin_shape() {
    let owner = TrustedInteractionOwner::host("peri/cron/scheduled-trigger", 7);

    assert_eq!(owner.kind(), "host");
    assert_eq!(owner.session_id(), None);
    assert!(owner.model_turn().is_none());
    assert_eq!(
        owner.to_string(),
        "host(namespace=peri/cron/scheduled-trigger, seq=7)"
    );
}

#[test]
fn owner_round_trips_through_wire_shape() {
    let owner = TrustedInteractionOwner::model("session-1", binding());
    let json = serde_json::to_value(&owner).expect("owner serializes");
    assert_eq!(json["kind"], "model");
    assert_eq!(
        serde_json::from_value::<TrustedInteractionOwner>(json).expect("owner deserializes"),
        owner
    );

    let app = TrustedInteractionOwner::app("app-1", "att-1", ConnectionEpoch::new(3));
    let json = serde_json::to_value(&app).expect("owner serializes");
    assert_eq!(json["kind"], "app");
    assert_eq!(json["connectionEpoch"], 3);
    assert_eq!(
        serde_json::from_value::<TrustedInteractionOwner>(json).expect("owner deserializes"),
        app
    );
}

#[test]
fn no_online_endpoint_is_a_distinct_cause() {
    assert_eq!(
        serde_json::to_value(UnansweredCause::NoOnlineEndpoint).expect("cause serializes"),
        serde_json::json!("no_online_endpoint")
    );
    assert_ne!(
        UnansweredCause::NoOnlineEndpoint.reason_text(),
        UnansweredCause::NonInteractiveClient.reason_text()
    );
    // 既有词表不变：旧客户端声明与未知取值仍按原语义解析。
    let mut meta = serde_json::Map::new();
    meta.insert(
        ELICITATION_UNANSWERED_META_KEY.to_string(),
        serde_json::json!("non_interactive_client"),
    );
    assert_eq!(
        UnansweredCause::from_meta(Some(&meta)),
        Some(UnansweredCause::NonInteractiveClient)
    );
    let (key, value) = UnansweredCause::NoOnlineEndpoint.meta_entry();
    meta.insert(key, value);
    assert_eq!(
        UnansweredCause::from_meta(Some(&meta)),
        Some(UnansweredCause::NoOnlineEndpoint)
    );
}

/// 诚实性：三个本端铸造的原因**互不重叠**，且都不得声称别的分支的事实
/// （「有在线 endpoint 但归属不符」不得被说成「没有任何在线 endpoint」）。
#[test]
fn router_minted_causes_are_distinct_and_truthful() {
    for (cause, wire) in [
        (UnansweredCause::NoOnlineEndpoint, "no_online_endpoint"),
        (UnansweredCause::OwnerUnmatched, "owner_unmatched"),
        (UnansweredCause::AttachmentDetached, "attachment_detached"),
    ] {
        assert_eq!(
            serde_json::to_value(cause).expect("cause serializes"),
            serde_json::json!(wire),
            "{cause:?} 的 wire 值必须稳定"
        );
        // 新变体进出同一词表：`_meta` 往返不丢事实。
        let (key, value) = cause.meta_entry();
        let mut meta = serde_json::Map::new();
        meta.insert(key, value);
        assert_eq!(UnansweredCause::from_meta(Some(&meta)), Some(cause));
    }
    // 三者文本互不相同，且没有互相挪用事实。
    assert_ne!(
        UnansweredCause::OwnerUnmatched.reason_text(),
        UnansweredCause::NoOnlineEndpoint.reason_text()
    );
    assert!(
        !UnansweredCause::OwnerUnmatched
            .reason_text()
            .contains("没有任何在线"),
        "归属不符分支里在线 endpoint 存在，不得声称没有任何在线 endpoint"
    );
    assert!(
        !UnansweredCause::AttachmentDetached
            .reason_text()
            .contains("没有任何在线"),
        "附件 detach 分支陈述的是投递资格缺失，不是「本端没有 endpoint」"
    );
    for cause in [
        UnansweredCause::OwnerUnmatched,
        UnansweredCause::AttachmentDetached,
    ] {
        let text = cause.reason_text();
        assert!(
            text.contains("未执行") && text.contains("不是用户拒绝"),
            "{cause:?} 必须保持 fail-closed 且不呈现为用户拒绝：{text}"
        );
    }
}

// ─── InteractionAttribution：归属事实的唯一事实源 ──────────────────────────────

#[test]
fn app_origin_derives_attachment_attribution_and_owner() {
    use crate::tool_invocation::InvocationOrigin;

    let origin = InvocationOrigin::App {
        app_id: "app-1".into(),
        attachment_id: Some(AttachmentId::new("att-1")),
        connection_epoch: Some(ConnectionEpoch::new(3)),
        server_generation: 9,
    };
    let attribution = InteractionAttribution::from_invocation_origin(&origin)
        .expect("已附着的 App 来源必须能派生归属");
    assert_eq!(attribution.kind(), "app");
    assert_eq!(attribution.session_id(), None);
    assert_eq!(
        attribution,
        InteractionAttribution::app_attachment("app-1", "att-1", ConnectionEpoch::new(3))
    );
    // 归属 → owner 只有一条映射：owner 必须与归属逐字一致。
    assert_eq!(
        attribution.owner(),
        TrustedInteractionOwner::app("app-1", "att-1", ConnectionEpoch::new(3))
    );
}

#[test]
fn unattached_app_origin_has_no_attribution() {
    use crate::tool_invocation::InvocationOrigin;

    // 已签发但尚未附着连接：没有可路由的 endpoint ⇒ 无归属（fail-closed），
    // 不得伪造 attachment（例如用空串占位）。
    let origin = InvocationOrigin::App {
        app_id: "app-1".into(),
        attachment_id: None,
        connection_epoch: None,
        server_generation: 9,
    };
    assert_eq!(
        InteractionAttribution::from_invocation_origin(&origin),
        None
    );
}

#[test]
fn host_origin_reuses_invocation_origin_fields() {
    use crate::tool_invocation::InvocationOrigin;

    let attribution = InteractionAttribution::from_invocation_origin(&InvocationOrigin::Host {
        namespace: "peri/mcp/invoke".into(),
        seq: 7,
    })
    .expect("Host 来源必须能派生归属");
    assert_eq!(attribution.kind(), "host");
    assert_eq!(
        attribution.owner(),
        TrustedInteractionOwner::host("peri/mcp/invoke", 7)
    );
}

#[test]
fn model_and_nested_origins_do_not_fabricate_attribution() {
    use crate::tool_invocation::{InvocationId, InvocationOrigin, ToolCallId};

    // 模型 / 嵌套来源不含 session 与 turn 绑定：必须显式用 `model_turn` 提供
    // 真实 turn 事实，不得从 tool-call ID 反推归属。
    assert_eq!(
        InteractionAttribution::from_invocation_origin(&InvocationOrigin::Model {
            tool_call_id: ToolCallId::new("call-1"),
        }),
        None
    );
    assert_eq!(
        InteractionAttribution::from_invocation_origin(&InvocationOrigin::Nested {
            parent_invocation_id: InvocationId::new("parent-1"),
        }),
        None
    );
}

#[test]
fn model_turn_attribution_maps_to_model_owner_without_reinterpretation() {
    let binding = binding();
    let attribution = InteractionAttribution::model_turn("session-1", binding.clone());

    assert_eq!(attribution.kind(), "model");
    assert_eq!(attribution.session_id(), Some("session-1"));
    let owner = attribution.owner();
    let (session_id, turn_id, attempt_id) = owner
        .model_turn()
        .expect("模型归属的 owner 必须携带 turn 事实");
    assert_eq!(session_id, "session-1");
    assert_eq!(turn_id, &binding.turn_id);
    assert_eq!(attempt_id, &binding.attempt_id);
}

#[test]
fn attribution_round_trips_through_wire_shape() {
    let attribution =
        InteractionAttribution::app_attachment("app-1", "att-1", ConnectionEpoch::initial());
    let json = serde_json::to_value(&attribution).expect("attribution serializes");
    assert_eq!(json["kind"], "appAttachment");
    assert_eq!(json["appId"], "app-1");
    assert_eq!(json["attachmentId"], "att-1");
    assert_eq!(
        serde_json::from_value::<InteractionAttribution>(json).expect("attribution deserializes"),
        attribution
    );
}

// ─── MCP elicitation contract ────────────────────────────────────────────────

#[test]
fn mcp_elicitation_form_round_trips_without_losing_schema() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "approve": { "type": "boolean", "title": "Approve" },
            "note": { "type": "string", "description": "optional note" }
        },
        "required": ["approve"]
    });
    let context = InteractionContext::Elicitation {
        request: ElicitationRequest::Form {
            server: "expense".into(),
            tool: "submit_expense".into(),
            message: "Approve this expense?".into(),
            requested_schema: schema.clone(),
        },
    };
    let json = serde_json::to_value(&context).expect("elicitation context serializes");
    assert_eq!(json["kind"], "Elicitation");
    assert_eq!(json["request"]["mode"], "form");
    assert_eq!(json["request"]["server"], "expense");
    assert_eq!(json["request"]["requested_schema"], schema);

    let decoded: InteractionContext =
        serde_json::from_value(json).expect("elicitation context deserializes");
    let InteractionContext::Elicitation {
        request: ElicitationRequest::Form {
            requested_schema, ..
        },
    } = decoded
    else {
        panic!("expected form elicitation")
    };
    assert_eq!(requested_schema, schema);
}

#[test]
fn mcp_elicitation_url_keeps_2025_11_25_id_and_allows_0728_absence() {
    let with_id = serde_json::json!({
        "kind": "Elicitation",
        "request": {
            "mode": "url",
            "server": "oauth",
            "tool": "connect",
            "message": "Authorize in browser",
            "url": "https://example.com/connect",
            "elicitation_id": "e-1"
        }
    });
    let decoded: InteractionContext =
        serde_json::from_value(with_id).expect("url elicitation deserializes");
    let InteractionContext::Elicitation {
        request: ElicitationRequest::Url { elicitation_id, .. },
    } = decoded
    else {
        panic!("expected url elicitation")
    };
    assert_eq!(elicitation_id, "e-1");

    // 2026-07-28 移除了 elicitationId；缺字段必须可解码（空串表示不存在）。
    let without_id = serde_json::json!({
        "kind": "Elicitation",
        "request": {
            "mode": "url",
            "server": "oauth",
            "tool": "connect",
            "message": "Authorize in browser",
            "url": "https://example.com/connect"
        }
    });
    let decoded: InteractionContext =
        serde_json::from_value(without_id).expect("0728 url elicitation deserializes");
    let InteractionContext::Elicitation {
        request: ElicitationRequest::Url { elicitation_id, .. },
    } = decoded
    else {
        panic!("expected url elicitation")
    };
    assert_eq!(elicitation_id, "");
}

#[test]
fn mcp_elicitation_decisions_keep_three_actions_distinct() {
    let accept = InteractionResponse::Elicitation(ElicitationDecision::Accept {
        content: serde_json::json!({ "approve": false }),
    });
    let json = serde_json::to_value(&accept).expect("accept serializes");
    assert_eq!(json["kind"], "Elicitation");
    assert_eq!(json["action"], "accept");
    assert_eq!(json["content"], serde_json::json!({ "approve": false }));
    let decoded: InteractionResponse = serde_json::from_value(json).expect("accept deserializes");
    assert!(matches!(
        decoded,
        InteractionResponse::Elicitation(ElicitationDecision::Accept { content })
            if content == serde_json::json!({ "approve": false })
    ));

    for (decision, action) in [
        (ElicitationDecision::Decline, "decline"),
        (ElicitationDecision::Cancel, "cancel"),
    ] {
        let response = InteractionResponse::Elicitation(decision);
        let json = serde_json::to_value(&response).expect("decision serializes");
        assert_eq!(json["action"], action);
        let decoded: InteractionResponse =
            serde_json::from_value(json).expect("decision deserializes");
        match (action, decoded) {
            ("decline", InteractionResponse::Elicitation(ElicitationDecision::Decline)) => {}
            ("cancel", InteractionResponse::Elicitation(ElicitationDecision::Cancel)) => {}
            (expected, other) => panic!("expected {expected}, got {other:?}"),
        }
    }
}
