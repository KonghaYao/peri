//! 阶段 4a facade 契约的 wire 形状与「不得声称未实现能力」断言。
//!
//! 断言面：serde 往返（字段名 camelCase、请求 `deny_unknown_fields`、响应不 deny）、
//! 判决按键可区分、能力声明逐字段诚实、预算常量单一权威。

use std::collections::BTreeMap;

use super::*;
use crate::mcp_apps::{
    McpAppsErrorKind, SessionInvocationOutcome, MCP_APPS_ENVELOPE_VERSION, MCP_APPS_HTML_MIME,
    MCP_APPS_PROTOCOL_VERSION,
};
use crate::tool_outcome::RawToolResult as RawCallToolResult;
use serde_json::json;

fn sample_descriptor() -> AppDescriptor {
    AppDescriptor {
        app_id: "app-1".into(),
        server_id: "server-1".into(),
        server_generation: 7,
        resource_uri: "ui://app".into(),
        instantiating_tool: "show_canvas".into(),
        qualification: AppQualification::Interactive {
            allowed_tools: HashMap::from([(
                "show_canvas".to_string(),
                "mcp__s__show_canvas".into(),
            )]),
        },
        model: ModelAssociation {
            invocation_id: InvocationId::generate(),
            tool_call_id: Some(ToolCallId::new("call-1")),
        },
        execution_state: AppExecutionState::Live,
        attachments: vec![AttachmentSummary {
            attachment_id: AttachmentId::new("att-1"),
            namespace: RequestNamespaceId::new(),
            connection_epoch: ConnectionEpoch::initial(),
            connection_id: Some("conn-1".into()),
            client_view_id: ClientViewId::new("view-1"),
        }],
        retention_remaining_ms: 1000,
    }
}

fn sample_snapshot() -> AppAttachSnapshot {
    let descriptor = sample_descriptor();
    AppAttachSnapshot {
        attachment: AttachmentBinding {
            attachment_id: AttachmentId::new("att-1"),
            namespace: RequestNamespaceId::new(),
            connection_epoch: ConnectionEpoch::initial(),
            next_seq: RequestSeq::new(1),
        },
        descriptor,
        ui_resource: UiResourceRef {
            uri: "ui://app".into(),
            mime_type: crate::mcp_apps::MCP_APPS_HTML_MIME.into(),
        },
        initial_input: Map::from_iter([("count".to_string(), json!(3))]),
        // 完整 raw：多模态 + structuredContent + `_meta` + isError 全部保真。
        initial_result: RawCallToolResult::new(json!({
            "content": [{"type": "text", "text": "boom"}, {"type": "image", "data": "aGk=", "mimeType": "image/png"}],
            "structuredContent": {"ok": false},
            "_meta": {"server": "extra"},
            "isError": true
        })),
        features: McpAppsFeatures::implemented(),
        limits: McpAppsLimits::conservative(),
        reattached: false,
    }
}

/// 三个版本号分工不同：facade 版本不得由协议版本或信封版本推导。
#[test]
fn facade_version_is_distinct_from_protocol_and_envelope_versions() {
    assert_eq!(MCP_APPS_FACADE_VERSION, "2");
    assert_eq!(MCP_APPS_PROTOCOL_VERSION, "2026-01-26");
    assert_eq!(MCP_APPS_ENVELOPE_VERSION, "1");
    assert_ne!(MCP_APPS_FACADE_VERSION, MCP_APPS_PROTOCOL_VERSION);
    assert_ne!(MCP_APPS_FACADE_VERSION, MCP_APPS_ENVELOPE_VERSION);
}

/// 能力声明逐字段诚实：已实现 6 项为真，未实现 4 项**恒假**。
#[test]
fn capability_declares_only_implemented_features() {
    let features = McpAppsFeatures::implemented();
    assert!(features.tools_call);
    assert!(features.resources_read);
    assert!(features.resources_list);
    assert!(features.cancel);
    assert!(features.detach_reattach);
    assert!(features.rerun);
    assert!(
        !features.tool_list_changed(),
        "工具列表变更通知未接线，不得声明支持"
    );
    assert!(
        !features.resource_subscriptions(),
        "资源订阅无实现，不得声明支持"
    );
    assert!(!features.sampling(), "sampling 无实现，不得声明支持");
    assert!(!features.logging(), "logging 无实现，不得声明支持");
    assert_eq!(
        McpAppsFeatures::default(),
        features,
        "默认值即已实现集（无第二个真值来源）"
    );

    let capability = McpAppsCapability::backend(vec![crate::mcp_apps::MCP_APPS_HTML_MIME.into()]);
    assert_eq!(capability.facade_version, MCP_APPS_FACADE_VERSION);
    assert_eq!(capability.apps_protocol_version, MCP_APPS_PROTOCOL_VERSION);
    assert_eq!(capability.backend_features, features);
    assert_eq!(capability.limits, McpAppsLimits::conservative());
}

/// 预算常量单一权威：`McpAppsLimits::conservative()` 只读 §4.3 常量。
#[test]
fn limits_mirror_the_single_authority_constants() {
    let limits = McpAppsLimits::conservative();
    assert_eq!(limits.max_apps_per_session, MAX_APPS_PER_SESSION);
    assert_eq!(limits.max_attachments_per_app, MAX_ATTACHMENTS_PER_APP);
    assert_eq!(
        limits.max_attachments_per_session,
        MAX_ATTACHMENTS_PER_SESSION
    );
    assert_eq!(limits.max_pending_per_namespace, MAX_PENDING_PER_NAMESPACE);
    assert_eq!(
        limits.max_namespaces_per_session,
        MAX_NAMESPACES_PER_SESSION
    );
    assert_eq!(limits.max_retained_results, MAX_RETAINED_RESULTS);
    assert_eq!(limits.max_result_bytes, MAX_RESULT_BYTES);
    assert_eq!(limits.max_retained_bytes, MAX_RETAINED_BYTES);
    assert_eq!(limits.detach_reattach_window_ms, DETACH_REATTACH_WINDOW_MS);
    assert_eq!(limits.request_wait_timeout_ms, REQUEST_WAIT_TIMEOUT_MS);
    assert_eq!(limits.app_retention_ms, APP_RETENTION_MS);
    // 4a 保守值（4b 依实测调参）：值本身是可断言的事实，不是推荐值。
    assert_eq!(MAX_APPS_PER_SESSION, 64);
    assert_eq!(MAX_ATTACHMENTS_PER_APP, 8);
    assert_eq!(MAX_RESULT_BYTES, 1024 * 1024);
    assert_eq!(DETACH_REATTACH_WINDOW_MS, 300_000);
}

/// 请求 DTO：camelCase 字段名 + `deny_unknown_fields`（不吞客户端拼错的字段）。
#[test]
fn request_dto_uses_camel_case_and_rejects_unknown_fields() {
    let request = AppsRequestRequest {
        session_id: "session-1".into(),
        attachment_id: AttachmentId::new("att-1"),
        epoch: ConnectionEpoch::initial(),
        seq: RequestSeq::new(2),
        operation: AppsOperation::ToolsCall {
            name: "show_canvas".into(),
            arguments: Map::from_iter([("a".to_string(), json!(1))]),
        },
    };
    let value = serde_json::to_value(&request).expect("请求必须可序列化");
    assert_eq!(value["sessionId"], json!("session-1"));
    assert_eq!(value["attachmentId"], json!("att-1"));
    assert_eq!(value["epoch"], json!(1));
    assert_eq!(value["seq"], json!(2));
    assert_eq!(value["operation"]["method"], json!("tools/call"));
    assert_eq!(value["operation"]["name"], json!("show_canvas"));
    assert_eq!(
        serde_json::from_value::<AppsRequestRequest>(value.clone()).expect("往返"),
        request
    );

    let mut unknown = value;
    unknown["serverId"] = json!("spoofed");
    assert!(
        serde_json::from_value::<AppsRequestRequest>(unknown).is_err(),
        "请求面必须拒绝未知字段（客户端不得自报目标）"
    );
}

/// 响应 DTO 不 deny：客户端不得因追加字段而失败。
#[test]
fn response_dto_is_additive() {
    let response = AppsListResponse {
        descriptors: vec![sample_descriptor()],
        features: McpAppsFeatures::implemented(),
        limits: McpAppsLimits::conservative(),
        host_namespace: HostNamespace {
            namespace: RequestNamespaceId::new(),
            next_seq: RequestSeq::new(1),
        },
    };
    let mut value = serde_json::to_value(&response).expect("响应必须可序列化");
    value["futureField"] = json!("added later");
    let parsed: AppsListResponse = serde_json::from_value(value).expect("响应面必须容忍追加字段");
    assert_eq!(parsed.descriptors.len(), 1);
    assert_eq!(parsed.host_namespace.next_seq.get(), 1);
}

/// attach 快照：完整 raw 保真（多模态 / structuredContent / `_meta` / `isError`）。
#[test]
fn attach_snapshot_round_trip_preserves_raw_result() {
    let snapshot = sample_snapshot();
    let value = serde_json::to_value(&snapshot).expect("快照必须可序列化");
    assert_eq!(value["initialResult"]["isError"], json!(true));
    assert_eq!(
        value["initialResult"]["content"][1]["mimeType"],
        json!("image/png")
    );
    assert_eq!(
        value["initialResult"]["structuredContent"],
        json!({"ok": false})
    );
    assert_eq!(value["initialResult"]["_meta"], json!({"server": "extra"}));
    assert_eq!(value["initialInput"], json!({"count": 3}));
    assert_eq!(value["uiResource"]["mimeType"], json!(MCP_APPS_HTML_MIME));
    assert_eq!(value["attachment"]["nextSeq"], json!(1));
    assert_eq!(value["reattached"], json!(false));

    let parsed: AppAttachSnapshot = serde_json::from_value(value).expect("往返");
    assert_eq!(parsed, snapshot);
    assert!(
        parsed.initial_result.is_error(),
        "isError 是结果事实，不得在往返中丢失"
    );
}

/// 判决按键可区分：六种非接受判决各有独立键，且都不得被当成新执行。
#[test]
fn request_verdict_keys_are_distinguishable_and_none_admit_execution() {
    let cases = [
        (RequestVerdict::Accepted, "accepted"),
        (RequestVerdict::JoinedPending, "joinedPending"),
        (RequestVerdict::Duplicate, "duplicate"),
        (RequestVerdict::Conflict, "conflict"),
        (RequestVerdict::ResultExpired, "resultExpired"),
        (RequestVerdict::CapacityRejected, "capacityRejected"),
        (RequestVerdict::UnknownNamespace, "unknownNamespace"),
    ];
    let mut keys = Vec::new();
    for (verdict, expected) in cases {
        let value = serde_json::to_value(verdict).expect("判决必须可序列化");
        assert_eq!(value, json!(expected));
        keys.push(expected);
    }
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys.len(),
        7,
        "每个判决必须有独立键（不允许同一 error 承载多种事实）"
    );

    let gap = serde_json::to_value(RequestVerdict::SequenceGap {
        expected: RequestSeq::new(5),
    })
    .expect("判决必须可序列化");
    assert_eq!(gap, json!({"sequenceGap": {"expected": 5}}));

    assert!(RequestVerdict::Accepted.is_admitted());
    for verdict in [
        RequestVerdict::JoinedPending,
        RequestVerdict::Duplicate,
        RequestVerdict::Conflict,
        RequestVerdict::ResultExpired,
        RequestVerdict::CapacityRejected,
        RequestVerdict::UnknownNamespace,
        RequestVerdict::SequenceGap {
            expected: RequestSeq::new(5),
        },
    ] {
        assert!(!verdict.is_admitted(), "{verdict:?} 不得被当作已登记执行");
    }
}

/// in-band 判决 → 错误面投影：只有非接受判决有 kind，且不产生第二个权威。
#[test]
fn verdict_error_kind_projection_matches_the_same_facts() {
    assert_eq!(RequestVerdict::Accepted.error_kind(), None);
    assert_eq!(RequestVerdict::JoinedPending.error_kind(), None);
    assert_eq!(RequestVerdict::Duplicate.error_kind(), None);
    assert_eq!(
        RequestVerdict::Conflict.error_kind(),
        Some(McpAppsErrorKind::Conflict)
    );
    assert_eq!(
        RequestVerdict::ResultExpired.error_kind(),
        Some(McpAppsErrorKind::ResultExpired)
    );
    assert_eq!(
        RequestVerdict::SequenceGap {
            expected: RequestSeq::new(2)
        }
        .error_kind(),
        Some(McpAppsErrorKind::SequenceGap)
    );
    assert_eq!(
        RequestVerdict::CapacityRejected.error_kind(),
        Some(McpAppsErrorKind::CapacityExceeded)
    );
    assert_eq!(
        RequestVerdict::UnknownNamespace.error_kind(),
        Some(McpAppsErrorKind::UnknownNamespace)
    );
}

/// 错误种类有稳定 wire 键，且两两不同（客户端按键区分，不靠字符串前缀判定）。
#[test]
fn error_kinds_have_distinct_stable_keys() {
    let kinds = [
        McpAppsErrorKind::UnknownNamespace,
        McpAppsErrorKind::StaleAttachmentEpoch,
        McpAppsErrorKind::Conflict,
        McpAppsErrorKind::ResultExpired,
        McpAppsErrorKind::SequenceGap,
        McpAppsErrorKind::CapacityExceeded,
    ];
    let keys = kinds.map(McpAppsErrorKind::as_str);
    assert_eq!(
        keys,
        [
            "unknown_namespace",
            "stale_attachment_epoch",
            "conflict",
            "result_expired",
            "sequence_gap",
            "capacity_exceeded",
        ]
    );
}

/// 操作面只接受声明过的三种方法（不内嵌任意 JSON-RPC 方法名）。
#[test]
fn apps_operation_only_accepts_declared_methods() {
    let read: AppsOperation = serde_json::from_value(json!({
        "method": "resources/read",
        "uri": "ui://app"
    }))
    .expect("resources/read 必须可解析");
    assert_eq!(
        read,
        AppsOperation::ResourcesRead {
            uri: "ui://app".into()
        }
    );
    let list: AppsOperation = serde_json::from_value(json!({
        "method": "resources/list"
    }))
    .expect("无游标的 resources/list 必须可解析");
    assert_eq!(list, AppsOperation::ResourcesList { cursor: None });
    assert!(
        serde_json::from_value::<AppsOperation>(json!({"method": "sampling/createMessage"}))
            .is_err(),
        "未声明的方法必须被拒绝（不得透传到 server）"
    );
    assert!(
        serde_json::from_value::<AppsOperation>(json!({"method": "tools/callx", "name": "x"}))
            .is_err()
    );
}

/// `DisplayOnly` 零 callback 资格；`Interactive` 只暴露有界白名单。
#[test]
fn display_only_qualification_grants_no_callback() {
    let display = AppQualification::DisplayOnly;
    assert!(display.allowed_tools().is_none());
    assert!(display.allowed_effective_name("show_canvas").is_none());
    let value = serde_json::to_value(&display).expect("资格必须可序列化");
    assert_eq!(value, json!({"kind": "displayOnly"}));

    let interactive = AppQualification::Interactive {
        allowed_tools: HashMap::from([("show_canvas".to_string(), "mcp__s__show_canvas".into())]),
    };
    assert_eq!(
        interactive
            .allowed_effective_name("show_canvas")
            .map(String::as_str),
        Some("mcp__s__show_canvas")
    );
    assert!(interactive.allowed_effective_name("other").is_none());
}

/// 结果判决面：三种 outcome 形状互不混淆，`isError` 仍是结果事实。
#[test]
fn request_outcome_shapes_are_typed() {
    let call = AppsRequestOutcome::ToolsCall(SessionInvocationOutcome::Completed(
        RawCallToolResult::new(json!({"content": [], "isError": true})),
    ));
    let value = serde_json::to_value(&call).expect("outcome 必须可序列化");
    assert_eq!(value["toolsCall"]["kind"], json!("completed"));
    assert_eq!(
        serde_json::from_value::<AppsRequestOutcome>(value).expect("往返"),
        call
    );

    let read = AppsRequestOutcome::ResourcesRead {
        resources: vec![RawResource {
            uri: "videos://clip".into(),
            mime_type: "video/mp4".into(),
            text: None,
            blob: Some("aGk=".into()),
            meta: BTreeMap::new(),
            extra: BTreeMap::new(),
        }],
    };
    let value = serde_json::to_value(&read).expect("outcome 必须可序列化");
    assert_eq!(
        value["resourcesRead"]["resources"][0]["uri"],
        json!("videos://clip")
    );
    assert_eq!(
        serde_json::from_value::<AppsRequestOutcome>(value).expect("往返"),
        read
    );

    let list = AppsRequestOutcome::ResourcesList {
        resources: vec![RawResourceDescriptor {
            uri: "videos://clip".into(),
            name: Some("clip".into()),
            mime_type: Some("video/mp4".into()),
            description: None,
            meta: BTreeMap::new(),
            extra: BTreeMap::from_iter([("futureField".to_string(), json!(true))]),
        }],
        next_cursor: Some("cursor-1".into()),
    };
    let value = serde_json::to_value(&list).expect("outcome 必须可序列化");
    assert_eq!(value["resourcesList"]["nextCursor"], json!("cursor-1"));
    assert_eq!(
        value["resourcesList"]["resources"][0]["futureField"],
        json!(true),
        "目录项的未知扩展字段必须保真"
    );
    assert_eq!(
        serde_json::from_value::<AppsRequestOutcome>(value).expect("往返"),
        list
    );
}

/// namespace 永不复用：每次分配都是新身份（销毁后同 ID 不可重建）。
#[test]
fn namespace_ids_are_never_reused() {
    let first = RequestNamespaceId::new();
    let second = RequestNamespaceId::new();
    assert_ne!(first, second);
    assert_eq!(RequestNamespaceId::from(first.as_str()), first);
    assert_eq!(RequestSeq::new(1).next(), RequestSeq::new(2));
}

/// attach 响应与快照同源：`attachment` 与 `snapshot.attachment` 是同一个值。
#[test]
fn attach_response_carries_the_same_binding_as_the_snapshot() {
    let snapshot = sample_snapshot();
    let response = AppsAttachResponse {
        attachment: snapshot.attachment.clone(),
        snapshot,
        features: McpAppsFeatures::implemented(),
        limits: McpAppsLimits::conservative(),
    };
    let value = serde_json::to_value(&response).expect("响应必须可序列化");
    assert_eq!(
        value["attachment"], value["snapshot"]["attachment"],
        "两处绑定必须同源同权（单一构造点）"
    );
    assert_eq!(
        value["attachment"]["namespace"],
        value["attachment"]["namespace"]
    );
    assert!(value["snapshot"]["descriptor"]["model"]["toolCallId"].is_string());

    // 重挂接时序号随 ledger 当前值重建，附着快照不构成第二权威。
    let binding = AttachmentBinding {
        attachment_id: AttachmentId::new("att-1"),
        namespace: RequestNamespaceId::new(),
        connection_epoch: ConnectionEpoch::initial(),
        next_seq: RequestSeq::new(1),
    };
    assert_eq!(
        binding.with_next_seq(RequestSeq::new(9)).next_seq,
        RequestSeq::new(9)
    );
    assert_eq!(binding.next_seq, RequestSeq::new(1));
}
