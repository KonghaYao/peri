use super::*;
use peri_acp_types::interaction::ConnectionEpoch;
use peri_acp_types::tool_invocation::InvocationId;
use peri_acp_types::tool_outcome::RawToolResult;

/// record 的运行时世代（夹具）。
const INCARNATION: u64 = 3;

/// 签发一条记录（测试夹具）：可交互资格 + 完整初始结果 + 模型关联。
fn record(tool: &str, token: &str, generation: u64) -> Arc<McpAppRecord> {
    Arc::new(McpAppRecord::new(record_spec(tool, token, generation)))
}

/// 签发事实（夹具）：资格可被具体用例覆盖（`..record_spec(..)`）。
fn record_spec(tool: &str, token: &str, generation: u64) -> McpAppRecordSpec {
    McpAppRecordSpec {
        owner_session_id: "session".to_string(),
        server_id: "server".to_string(),
        server_generation: generation,
        resource_uri: "ui://app".to_string(),
        instantiating_tool: tool.to_string(),
        invocation_id: InvocationId::generate(),
        invocation_token: token.to_string(),
        tool_call_id: Some(ToolCallId::new(format!("call-{token}"))),
        incarnation: INCARNATION,
        qualification: AppQualification::Interactive {
            allowed_tools: HashMap::from([(tool.to_string(), format!("mcp__server__{tool}"))]),
        },
        effective_input: Map::from_iter([("count".to_string(), Value::from(2))]),
        initial_result: Arc::new(RawToolResult::new(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "structuredContent": {"ok": true},
        }))),
    }
}

/// 附着一条连接路由（夹具；纪元取调用方值）。
fn route(connection: &str, attachment: &str) -> Arc<AttachmentRoute> {
    Arc::new(AttachmentRoute::new(
        connection,
        AttachmentId::new(attachment),
        RequestNamespaceId::new(),
        ConnectionEpoch::initial(),
        ClientViewId::new(connection),
    ))
}

/// 附着一条连接路由到**已签发**记录（生产顺序：登记世代 → 签发 → 按 record 身份附着）。
fn attach(
    registry: &McpAppBindingRegistry,
    record_id: &str,
    connection: &str,
) -> Option<Arc<McpAppRecord>> {
    let record = registry.find(record_id)?;
    let attached = route(connection, &record.record_id);
    record.attach(attached).then_some(record)
}

/// [回归] 记录是**纯数据**：不携带 dispatcher / 取消令牌 / 轮次世代；附着按
/// **record 身份**发生（无「领取即消费」的令牌语义——批 D/S8 删除 `claim` 后，
/// 记录不再因领取而从注册表消失）。
#[test]
fn record_is_pure_data_and_attaches_by_identity() {
    let registry = McpAppBindingRegistry::default();
    let issued = record("tool", "token-1", 7);
    registry
        .issue(Arc::clone(&issued))
        .expect("世代一致的记录必须可登记");

    let attached =
        attach(&registry, &issued.record_id, "connection").expect("已签发记录必须可附着");
    assert!(attached.is_attached(), "附着后记录进入 Attached");
    assert_eq!(
        attached.attachments().len(),
        1,
        "附着必须产生连接路由（按附件身份定位，无单附件访问器）"
    );
    assert_eq!(
        attached.entitlement,
        EntitlementState::Unenforced,
        "entitlement 事实必须在类型上承载（裁决 R4）"
    );
    assert!(
        Arc::ptr_eq(&attached, &issued),
        "附着返回的必须是同一记录实例（注册表不复制）"
    );
    assert!(
        registry.find(&issued.record_id).is_some(),
        "附着不得把记录从注册表摘除（`apps/list` 必须仍看得见它）"
    );
}

/// [新增断言] 记录承载完整签发事实：世代、资格、effective 输入、完整 raw、
/// 模型关联（AppDescriptor / snapshot 都从这些事实投影）。
#[test]
fn record_carries_the_full_issuance_facts() {
    let registry = McpAppBindingRegistry::default();
    let issued = record("tool", "token-1", 7);
    let invocation_id = issued.invocation_id.clone();
    registry.issue(Arc::clone(&issued)).expect("可登记");

    assert_eq!(issued.incarnation, INCARNATION, "签发时盖会话运行时世代");
    assert_eq!(
        issued.model().invocation_id,
        invocation_id,
        "模型关联的执行身份与签发身份同源"
    );
    assert_eq!(
        issued.model().tool_call_id.as_ref().map(ToolCallId::as_str),
        Some("call-token-1")
    );
    assert_eq!(
        issued.effective_input.get("count"),
        Some(&Value::from(2)),
        "snapshot 的初始输入是 effective（审批后）参数"
    );
    assert_eq!(
        issued
            .initial_result()
            .as_ref()
            .clone()
            .into_value()
            .get("structuredContent")
            .cloned(),
        Some(serde_json::json!({"ok": true})),
        "完整初始 raw 保真（structuredContent）"
    );
    assert!(
        issued.retention_remaining() > Duration::ZERO,
        "保留期是签发时起算的有界事实"
    );

    // `Arc` 共享：同一记录的结果不因多次读取而复制（设计 §9.1）。
    assert!(
        Arc::ptr_eq(issued.initial_result(), issued.initial_result()),
        "初始结果必须以同一 Arc 共享"
    );

    let attached = attach(&registry, &issued.record_id, "connection").expect("可附着");
    assert!(attached.allows_effective_name("mcp__server__tool"));
    assert!(!attached.allows_effective_name("mcp__server__other"));
    assert_eq!(
        attached.attachment_summaries().len(),
        1,
        "descriptor 投影包含当前附件"
    );
}

/// [新增断言] `DisplayOnly` 记录零 callback 资格（裁决 D5）。
#[test]
fn display_only_record_grants_no_callback_qualification() {
    let registry = McpAppBindingRegistry::default();
    // 展示资格独立于回调资格：记录可附着（有展示面），但没有可回调工具。
    let spec_record = Arc::new(McpAppRecord::new(McpAppRecordSpec {
        qualification: AppQualification::DisplayOnly,
        ..record_spec("model_tool", "token-display", 7)
    }));
    registry.issue(Arc::clone(&spec_record)).expect("可登记");

    assert!(spec_record.allowed_effective_name("model_tool").is_none());
    assert!(!spec_record.allows_effective_name("mcp__server__model_tool"));

    let attached = attach(&registry, &spec_record.record_id, "connection")
        .expect("仅可显示的记录仍可附着（展示资格独立于回调资格）");
    assert!(attached.is_attached());
    assert_eq!(
        attached.attachments().len(),
        1,
        "仅可显示的记录必须能附着（有展示面）"
    );
}

/// [新增断言] 世代不符的签发被显式拒绝（旧环境迟到签发不得入库）。
#[test]
fn issue_rejects_records_from_another_incarnation() {
    let registry = McpAppBindingRegistry::default();
    registry.register_session_incarnation("session", INCARNATION + 1);
    let rejection = registry
        .issue(record("tool", "token-1", 7))
        .expect_err("世代不符必须显式拒绝");
    assert_eq!(
        rejection,
        IssueRejection::IncarnationMismatch {
            bound: INCARNATION,
            current: INCARNATION + 1,
        }
    );
    assert!(
        registry
            .find(&registry.records_for_session("session").len().to_string())
            .is_none(),
        "被拒记录不得入库"
    );
    assert!(registry.records_for_session("session").is_empty());
    assert_eq!(
        registry.session_incarnation("session"),
        Some(INCARNATION + 1)
    );

    // 会话关闭清除世代登记（有界）。
    registry.revoke_session("session");
    assert_eq!(registry.session_incarnation("session"), None);
}

/// [新增断言] 多附件：一个 record 可有多个独立 View；detach 只影响目标附件，
/// 重挂接换纪元并撤销旧附件的投递资格（裁决 D6 / 设计 §8）。
#[test]
fn multiple_attachments_detach_and_rebind_are_per_attachment() {
    let registry = McpAppBindingRegistry::default();
    let issued = record("tool", "token-1", 7);
    registry.issue(Arc::clone(&issued)).expect("可登记");
    let record = registry
        .find(&issued.record_id)
        .expect("已签发记录必须可读");

    let first = route("connection-a", "att-a");
    let second = route("connection-b", "att-b");
    assert!(record.attach(Arc::clone(&first)), "第二个 View 可独立附着");
    assert!(record.attach(Arc::clone(&second)));
    assert_eq!(record.attachments().len(), 2);
    assert!(record.is_attached());

    // 同一个附件身份不得重复附着（必须走重挂接）。
    assert!(!record.attach(route("connection-a", "att-a")));

    // detach 一个附件：另一个仍然有效，record 不撤销。
    assert!(record.detach_attachment(&AttachmentId::new("att-a")));
    assert!(
        first.delivery.is_revoked(),
        "detach 必须撤销该附件的投递资格"
    );
    assert!(!second.delivery.is_revoked(), "其他附件不受影响");
    assert!(record.is_attached(), "仍有附件 ⇒ 记录可受理调用");
    assert!(
        !record.detach_attachment(&AttachmentId::new("att-a")),
        "detach 幂等"
    );
    assert_eq!(
        record
            .attachment_for(&AttachmentId::new("att-b"))
            .map(|route| route.connection_epoch),
        Some(ConnectionEpoch::initial())
    );

    // 重挂接：同一附件身份换新纪元（纪元由调用方递增），旧纪元令牌立即撤销。
    let rebind_target = route("connection-b", "att-b");
    let old_delivery = Arc::clone(&second.delivery);
    assert!(record.rebind_attachment(Arc::clone(&rebind_target)));
    assert!(old_delivery.is_revoked(), "重挂接必须撤销旧连接的投递资格");
    assert!(!rebind_target.delivery.is_revoked());
    assert_eq!(record.attachments().len(), 1, "重挂接替换而不是追加附件");
    assert!(
        !record.rebind_attachment(route("connection-c", "att-unknown")),
        "未知附件身份不得被重挂接（调用方应改用 attach）"
    );

    // 附件数上限（4a 保守值）：超限显式失败，不静默丢弃。
    let room = MAX_ATTACHMENTS_PER_APP - record.attachments().len() as u64;
    for index in 0..room {
        assert!(
            record.attach(route("connection-x", &format!("att-cap-{index}"))),
            "上限内的附件必须可附着"
        );
    }
    assert_eq!(record.attachments().len() as u64, MAX_ATTACHMENTS_PER_APP);
    assert!(
        !record.attach(route("connection-overflow", "att-overflow")),
        "超上限的附件必须显式拒绝"
    );
}

/// [回归] 撤销是终态且不取消执行：状态置 `Revoked`、**全部**附件交付令牌失效，
/// 且记录从索引移除（无 Arc 回环的可观察形式）。
#[test]
fn revocation_is_terminal_and_releases_the_record() {
    let registry = McpAppBindingRegistry::default();
    let issued = record("tool", "token-1", 7);
    registry.issue(Arc::clone(&issued)).expect("可登记");
    let attended = registry
        .find(&issued.record_id)
        .expect("已签发记录必须可读");
    let first = route("connection-a", "att-a");
    let second = route("connection-b", "att-b");
    assert!(attended.attach(Arc::clone(&first)));
    assert!(attended.attach(Arc::clone(&second)));
    let weak = Arc::downgrade(&attended);
    let record_id = attended.record_id.clone();
    drop(attended);
    // 夹具只保留注册表持有的一份（`issued` 是构造句柄，必须释放，
    // 否则本用例的 Weak 断言恒失败）。
    drop(issued);

    assert!(registry.revoke_app(&record_id), "撤销必须发生状态转移");
    assert!(!registry.revoke_app(&record_id), "重复撤销不再转移");
    assert!(
        first.delivery.is_revoked() && second.delivery.is_revoked(),
        "撤销必须失效 App 层与**全部**附件层交付资格"
    );
    assert!(
        registry.find(&record_id).is_none(),
        "撤销后的记录不可再寻址（也不被注册表持有）"
    );
    assert!(
        weak.upgrade().is_none(),
        "撤销 + 释放后记录必须可回收（注册表不留 Arc）"
    );
    assert!(
        registry
            .record_for_attachment(&AttachmentId::new("att-a"))
            .is_none(),
        "被撤销记录的附件不得再可寻址"
    );
}

/// [回归] 新 turn 不再撤销 App：记录跨轮有效，直到显式撤销 / 连接关闭。
#[test]
fn records_survive_across_turns_until_explicitly_revoked() {
    let registry = McpAppBindingRegistry::default();
    let issued = record("tool", "token-a", 7);
    registry.issue(Arc::clone(&issued)).expect("可登记");
    let first = registry
        .find(&issued.record_id)
        .expect("已签发记录必须可读");
    assert!(first.attach(route("connection-a", "att-a")));
    // 另一条记录（模拟下一轮实例化）：不得影响已有记录。
    registry
        .issue(record("tool", "token-b", 7))
        .expect("可登记");
    assert!(
        first.is_attached(),
        "新 turn 的实例化不得撤销已有 App record（设计 §4：新 turn 不撤销 App）"
    );
    assert!(registry.find(&first.record_id).is_some());
}

/// [回归] 连接关闭只撤销该连接的 Attachment（R7），且不取消执行。
#[test]
fn closing_one_connection_revokes_only_its_attachments() {
    let registry = McpAppBindingRegistry::default();
    let issued_a = record("tool", "token-a", 7);
    let issued_b = record("tool", "token-b", 7);
    registry.issue(Arc::clone(&issued_a)).expect("可登记");
    registry.issue(Arc::clone(&issued_b)).expect("可登记");
    let a = registry
        .find(&issued_a.record_id)
        .expect("已签发记录必须可读");
    assert!(a.attach(route("connection-a", "att-a")));
    // 同一条记录还有另一条连接上的附件：连接关闭只应撤销落在该连接上的附件。
    assert!(a.attach(route("connection-other", "att-other")));
    let b = registry
        .find(&issued_b.record_id)
        .expect("已签发记录必须可读");
    assert!(b.attach(route("connection-b", "att-b")));

    let revoked = registry.revoke_connection("connection-a");
    assert_eq!(revoked, vec![AttachmentId::new("att-a")]);
    assert!(registry.find(&a.record_id).is_none());
    assert!(
        registry.find(&b.record_id).is_some() && b.is_attached(),
        "其他连接的 App record 不受影响"
    );
    assert!(
        !b.delivery().is_revoked(),
        "关闭连接 A 不得撤销 B 的交付资格"
    );
}

/// [新增断言] detach 语义（裁决 D6 方向）：连接退出只撤销该连接的附件，
/// record 与**其他**连接的附件保持可受理（切换点留给后续批）。
#[test]
fn detach_connection_keeps_the_record_and_other_attachments_alive() {
    let registry = McpAppBindingRegistry::default();
    let issued = record("tool", "token-a", 7);
    registry.issue(Arc::clone(&issued)).expect("可登记");
    let a = registry
        .find(&issued.record_id)
        .expect("已签发记录必须可读");
    assert!(a.attach(route("connection-a", "att-a")));
    assert!(a.attach(route("connection-other", "att-other")));

    let detached = registry.detach_connection("connection-a");
    assert_eq!(detached, vec![AttachmentId::new("att-a")]);
    assert_eq!(
        a.state(),
        RecordState::Attached,
        "detach 不撤销 record（仍在有界重挂接窗口内）"
    );
    assert!(a.is_attached(), "其他连接的附件仍有效 ⇒ 记录仍可受理调用");
    assert!(registry.find(&a.record_id).is_some());
    assert!(a.attachment_for(&AttachmentId::new("att-a")).is_none());
    assert!(
        registry.detach_connection("connection-a").is_empty(),
        "重复 detach 不再产生状态转移"
    );
}

#[test]
fn deployment_presence_enables_apps_regardless_of_value() {
    assert!(deployment_profile(true).apps_enabled());
    assert!(!deployment_profile(false).apps_enabled());
}

#[test]
fn profile_only_negotiates_supported_mime() {
    let profile =
        McpCapabilityProfile::negotiated(["text/plain", MCP_APP_MIME_TYPE, MCP_APP_MIME_TYPE]);
    assert_eq!(
        profile.apps_mime_types().collect::<Vec<_>>(),
        [MCP_APP_MIME_TYPE]
    );
}

#[test]
fn negotiated_profile_builds_ui_extension() {
    let profile = McpCapabilityProfile::negotiated([MCP_APP_MIME_TYPE]);
    assert_eq!(
        profile.ui_extension(),
        Some(Map::from_iter([(
            "mimeTypes".to_string(),
            serde_json::json!([MCP_APP_MIME_TYPE]),
        )]))
    );
}

#[test]
fn disabled_profile_has_no_ui_extension() {
    assert!(McpCapabilityProfile::disabled().ui_extension().is_none());
}

#[test]
fn visibility_missing_defaults_to_model_and_app() {
    assert_eq!(
        ToolVisibility::from_tool_meta(None),
        ToolVisibility {
            model: true,
            app: true
        }
    );
}

#[test]
fn app_only_visibility_excludes_model() {
    let meta = serde_json::json!({"ui": {"visibility": ["app"]}});
    assert_eq!(
        ToolVisibility::from_tool_meta(Some(&meta)),
        ToolVisibility {
            model: false,
            app: true
        }
    );
}

#[test]
fn malformed_ui_metadata_fails_closed() {
    let meta = serde_json::json!({"ui": "invalid"});
    assert_eq!(
        ToolVisibility::from_tool_meta(Some(&meta)),
        ToolVisibility {
            model: false,
            app: false
        }
    );
}

#[test]
fn unknown_visibility_fails_closed() {
    let meta = serde_json::json!({"ui": {"visibility": ["app", "future"]}});
    assert_eq!(
        ToolVisibility::from_tool_meta(Some(&meta)),
        ToolVisibility {
            model: false,
            app: false
        }
    );
}

#[test]
fn conflicting_resource_uri_fails_closed() {
    let meta = serde_json::json!({
        "ui": {"resourceUri": "ui://canonical"},
        "ui/resourceUri": "ui://legacy"
    });
    assert_eq!(canonical_resource_uri(Some(&meta)), None);
}

#[test]
fn legacy_resource_uri_is_canonicalized() {
    let meta = serde_json::json!({"ui/resourceUri": "ui://app/index.html"});
    assert_eq!(
        canonical_resource_uri(Some(&meta)).as_deref(),
        Some("ui://app/index.html")
    );
}

#[test]
fn raw_result_round_trip_preserves_standard_and_unknown_fields() {
    let raw = serde_json::json!({
        "content": [{"type": "resource", "resource": {"uri": "ui://app", "mimeType": MCP_APP_MIME_TYPE, "text": "<html/>"}}],
        "structuredContent": {"answer": 42},
        "isError": false,
        "_meta": {"ui": {"domain": "example"}, "unknown": {"nested": true}}
    });
    let encoded = serde_json::to_string(&raw).unwrap();
    let decoded: RawToolResult = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.as_value(), &raw);
    assert_eq!(
        decoded.structured_content(),
        Some(&raw["structuredContent"])
    );
}

// ─── 4b 批 H：预算常量的边界（等于上限 ⇒ 通过 / 上限 + 1 ⇒ 既定行为）──────────
//
// 这三条用例是「广告 = 实现」的可执行证据：`McpAppsLimits` 广告给客户端的
// `max_apps_per_session` / `app_retention_ms` 由这些断言钉住。此前这两个字段
// 只有投影、没有任何强制点（实现侧实际生效的是另一套数字）。

/// 保留期 = 契约值 `APP_RETENTION_MS`，且到期边界恰好落在该值上（不是 300 s 旧值）。
#[test]
fn record_retention_is_the_contract_value_and_is_live_until_it() {
    let issued = record("tool", "token-retention", 7);
    let remaining = issued.retention_remaining();
    assert!(
        remaining <= Duration::from_millis(APP_RETENTION_MS),
        "保留期不得超过契约值（实测剩余 {remaining:?}）"
    );
    assert!(
        remaining > Duration::from_millis(APP_RETENTION_MS) - Duration::from_secs(1),
        "保留期必须等于契约值而非实现侧的旧值（实测剩余 {remaining:?}）"
    );

    // 到期边界（时钟注入；`peri_time` 无测试时钟，单调时钟推不动）：
    // 「刚好到期」⇒ 已不存活，「到期前 1 ms」⇒ 仍存活。
    let expires_at = issued.expires_at;
    assert!(
        issued.is_live_at(expires_at - Duration::from_millis(1)),
        "到期前的最后一刻仍存活"
    );
    assert!(
        !issued.is_live_at(expires_at),
        "到期即终态撤销的判据：`now == expires_at` 不算存活"
    );
    assert!(issued.is_live(), "刚签发的记录在生产时钟下存活");
}

/// 单会话上限（`MAX_APPS_PER_SESSION`）：等于上限 ⇒ 全部签发成功；上限 + 1 ⇒ 显式
/// 拒绝且**不淘汰**既有存活记录；容量按会话隔离、随撤销释放。
#[test]
fn session_capacity_rejects_the_record_over_the_contract_limit_without_evicting() {
    let registry = McpAppBindingRegistry::default();
    let mut issued: Vec<Arc<McpAppRecord>> = Vec::new();
    for index in 0..MAX_APPS_PER_SESSION {
        let spec = McpAppRecordSpec {
            owner_session_id: "session".to_string(),
            ..record_spec("tool", &format!("token-{index}"), 7)
        };
        issued.push(Arc::new(McpAppRecord::new(spec)));
        registry
            .issue(Arc::clone(issued.last().unwrap()))
            .expect("等于上限的每一条都必须签发成功（上限是闭区间：恰好 64 条允许）");
    }
    assert_eq!(
        registry.records_for_session("session").len() as u64,
        MAX_APPS_PER_SESSION
    );

    let overflow = registry
        .issue(record("tool", "token-overflow", 7))
        .expect_err("上限 + 1 必须显式拒绝");
    assert_eq!(
        overflow,
        IssueRejection::SessionCapacityExceeded {
            limit: MAX_APPS_PER_SESSION
        }
    );
    assert_eq!(
        registry.records_for_session("session").len() as u64,
        MAX_APPS_PER_SESSION,
        "拒绝不得淘汰既有记录（表保持在上限内）"
    );
    assert!(
        issued.iter().all(|record| record.is_live()),
        "既有存活记录一条都不许被淘汰（用户正在看的 App 不得静默消失）"
    );
    assert!(
        registry.find(&issued[0].record_id).is_some(),
        "最旧记录也在（拒绝策略与「跨会话容量淘汰」不同）"
    );

    // 上限是**会话级**：另一个会话不受影响。
    let other = Arc::new(McpAppRecord::new(McpAppRecordSpec {
        owner_session_id: "other-session".to_string(),
        ..record_spec("tool", "token-other", 7)
    }));
    registry
        .issue(other)
        .expect("会话级上限不跨会话累计（另一个会话仍可签发）");

    // 容量随撤销释放：撤销本会话一条后，上限内又腾出一格。
    registry.revoke_app(&issued[0].record_id);
    registry
        .issue(record("tool", "token-after-revoke", 7))
        .expect("已撤销记录不再占用会话容量");
}

/// 注册表级上限（`MAX_APP_RECORDS`，跨会话）：超出即淘汰**最旧**并终态撤销；
/// 新签发的记录存活。
#[test]
fn registry_capacity_evicts_the_oldest_record_across_sessions() {
    let registry = McpAppBindingRegistry::default();
    // 单会话上限 64 ⇒ 要跨过注册表上限必须用多个会话（这本身就是两条作用域的证明）：
    // 会话 = `index / MAX_APPS_PER_SESSION`，每个会话恰好 64 条（等于上限，允许）。
    let per_session = MAX_APPS_PER_SESSION as usize;

    let oldest = Arc::new(McpAppRecord::new(McpAppRecordSpec {
        owner_session_id: "s0".to_string(),
        ..record_spec("tool", "token-oldest", 7)
    }));
    registry.issue(Arc::clone(&oldest)).expect("最旧记录可签发");
    // record 身份是 UUID v7（毫秒时间戳 + 随机尾段）：跨毫秒才能保证「更旧」严格成立。
    std::thread::sleep(Duration::from_millis(2));

    let mut issued: Vec<Arc<McpAppRecord>> = vec![Arc::clone(&oldest)];
    for index in 1..=MAX_APP_RECORDS as usize {
        let spec = McpAppRecordSpec {
            owner_session_id: format!("s{}", index / per_session),
            ..record_spec("tool", &format!("token-{index}"), 7)
        };
        let record = Arc::new(McpAppRecord::new(spec));
        registry.issue(Arc::clone(&record)).expect("上限内可签发");
        issued.push(record);
    }

    assert_eq!(
        issued.iter().filter(|record| record.is_live()).count() as u64,
        MAX_APP_RECORDS,
        "注册表存活记录数不得超过上限"
    );
    assert_eq!(
        issued
            .iter()
            .filter(|record| record.state() == RecordState::Revoked)
            .count(),
        1,
        "超出部分恰好淘汰一条（FIFO 语义：一次只出一进一）"
    );
    assert_eq!(
        oldest.state(),
        RecordState::Revoked,
        "淘汰的是**最旧**记录（不是 `HashMap` 迭代序的任意一条）"
    );
    assert!(registry.find(&oldest.record_id).is_none(), "被淘汰记录出表");
    assert!(
        registry
            .find(&issued.last().unwrap().record_id)
            .is_some_and(|record| record.is_live()),
        "刚签发的记录必须存活（淘汰不得命中新记录）"
    );
}
