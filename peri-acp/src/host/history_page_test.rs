//! `session/load` 分流的参数规则（REQ §5.1~§5.3、验收 A25）。
//!
//! 这些用例只走 [`classify`]：模式是**请求参数 + 当前连接能力**的函数，
//! 每一次非法输入都必须得到显式拒绝，而不是退回 [`SessionLoadMode::InitialLoad`]。

use super::*;
use peri_acp_types::history_page::HistoryCursor;
use peri_acp_types::messages::MessageId;

const SESSION: &str = "session-a";

fn cursor_for(thread: &str) -> String {
    HistoryCursorEnvelope::new(
        0,
        format!("{thread}-snapshot"),
        HistoryCursor::new(
            ThreadId::from(thread.to_string()),
            MessageId::new(),
            format!("{thread}-head"),
        ),
    )
    .encode()
}

fn classify_page(params: Value, pagination: bool) -> Result<HistoryPageRequest, AcpError> {
    match classify(&params, pagination) {
        Ok(SessionLoadMode::HistoryPage(request)) => Ok(request),
        Ok(SessionLoadMode::InitialLoad) => panic!("request degraded into an initial load"),
        Err(error) => Err(error),
    }
}

#[test]
fn cursor_with_optional_limit_selects_the_history_page_mode() {
    let params = json!({ "sessionId": SESSION, "cursor": cursor_for(SESSION) });
    let request = classify_page(params, true).expect("default limit");
    assert_eq!(request.session_id, SESSION);
    assert_eq!(request.limit, DEFAULT_PAGE_LIMIT);
    assert_eq!(
        request.cursor.cursor.thread_id,
        ThreadId::from(SESSION.to_string())
    );

    for limit in [1u64, 2, 50, MAX_PAGE_LIMIT as u64] {
        let request = classify_page(
            json!({ "sessionId": SESSION, "cursor": cursor_for(SESSION), "limit": limit }),
            true,
        )
        .unwrap_or_else(|error| panic!("limit {limit} rejected: {error:?}"));
        assert_eq!(request.limit, limit as usize);
    }
}

#[test]
fn absent_cursor_field_is_an_initial_load_and_limit_alone_is_rejected() {
    assert_eq!(
        classify(&json!({ "sessionId": SESSION }), true).unwrap(),
        SessionLoadMode::InitialLoad
    );
    // 未协商分页能力的连接，只要不带分页字段，初始 load 照常（REQ §5.3）。
    assert_eq!(
        classify(&json!({ "sessionId": SESSION }), false).unwrap(),
        SessionLoadMode::InitialLoad
    );
    // limit 只属于历史页：单独出现在初始 load 上是坏请求，不静默忽略。
    let error = classify(&json!({ "sessionId": SESSION, "limit": 10 }), true).unwrap_err();
    assert_eq!(error.code, -32602);
}

#[test]
fn a_cursor_without_the_negotiated_capability_is_rejected() {
    let error = classify_page(
        json!({ "sessionId": SESSION, "cursor": cursor_for(SESSION) }),
        false,
    )
    .unwrap_err();
    assert_eq!(error.code, -32601, "能力门控与既有 rewind/oauth 同码");
}

#[test]
fn malformed_cursor_forms_are_rejected_not_downgraded() {
    for cursor in [
        json!(null),
        json!(""),
        json!(42),
        json!({ "v": 1 }),
        json!("not-a-cursor"),
        json!(r#"{"v":9,"thread":"session-a","anchor":"x","range":"y"}"#),
        // 外壳好、内层游标版本不支持：依旧是形式错误，不降级。
        Value::String(
            json!({
                "v": 1,
                "structure": 0,
                "snapshot": "snap",
                "cursor": r#"{"v":9,"thread":"session-a","anchor":"x","range":"y"}"#,
            })
            .to_string(),
        ),
    ] {
        let error =
            classify_page(json!({ "sessionId": SESSION, "cursor": cursor }), true).unwrap_err();
        assert_eq!(
            error.code, -32602,
            "cursor form {cursor} must be a bad request"
        );
    }
}

/// A19「旧 cursor 失效」：结构版本前进后，游标在**进入存储之前**就被判失效，
/// 与「锚点被删」得到同一个可区分错误（-32012）——客户端处置本来就是同一个。
#[test]
fn a_cursor_from_an_earlier_structure_version_expires_before_storage() {
    let envelope =
        HistoryCursorEnvelope::decode(&cursor_for(SESSION), &ThreadId::from(SESSION.to_string()))
            .expect("内置游标必须可解码");
    assert!(envelope.verify_structure_version(0).is_ok());

    let error = envelope
        .verify_structure_version(1)
        .expect_err("rewind 后的版本必须让旧游标失效");
    assert!(matches!(error, HistoryCursorError::Expired { .. }));
    assert_eq!(dispatch::history_page_error(error.into()).code, -32012);
}

/// `loadGeneration` 只属于初始 load，且必须是**非空字符串**：坏形状不得被当成「没带」
/// 而静默换成服务端新代次（那会让客户端暂存的旧代次永远等不到响应标记）。
#[test]
fn load_generation_is_accepted_only_on_the_initial_load_as_a_non_empty_string() {
    assert_eq!(
        initial_load_generation(&json!({ "loadGeneration": "gen-1" }))
            .unwrap()
            .map(|generation| generation.as_str().to_owned()),
        Some("gen-1".to_string())
    );
    assert!(initial_load_generation(&json!({})).unwrap().is_none());

    for bad in [json!(""), json!(7), json!(null), json!(["gen"])] {
        let error = classify(
            &json!({ "sessionId": SESSION, "loadGeneration": bad }),
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, -32602, "loadGeneration {bad} 必须拒绝");
    }
    // 历史页不重放，代次带上就是坏请求（不静默忽略）。
    let error = classify_page(
        json!({ "sessionId": SESSION, "cursor": cursor_for(SESSION), "loadGeneration": "gen" }),
        true,
    )
    .unwrap_err();
    assert_eq!(error.code, -32602);
}

#[test]
fn a_page_request_without_a_session_id_is_rejected() {
    let error = classify_page(json!({ "cursor": cursor_for(SESSION) }), true).unwrap_err();
    assert_eq!(error.code, -32602);
}

#[test]
fn a_cursor_from_another_session_is_its_own_error() {
    let error = classify_page(
        json!({ "sessionId": SESSION, "cursor": cursor_for("session-b") }),
        true,
    )
    .unwrap_err();
    assert_eq!(error.code, -32011, "跨会话与形式错误必须可区分");
}

#[test]
fn limits_outside_one_to_hundred_and_non_integers_are_rejected() {
    for limit in [
        json!(0),
        json!(101),
        json!(1000),
        json!("10"),
        json!(1.5),
        json!(null),
    ] {
        let error = classify_page(
            json!({ "sessionId": SESSION, "cursor": cursor_for(SESSION), "limit": limit }),
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, -32602, "limit {limit} must be rejected");
    }
}

#[test]
fn a_history_page_request_must_not_carry_session_setup_parameters() {
    for setup in [json!({ "cwd": "/tmp" }), json!({ "mcpServers": [] })] {
        let mut params = json!({ "sessionId": SESSION, "cursor": cursor_for(SESSION) });
        for (key, value) in setup.as_object().unwrap() {
            params[key.as_str()] = value.clone();
        }
        let error = classify_page(params, true).unwrap_err();
        assert_eq!(error.code, -32602, "setup 参数 {setup} 必须拒绝");
    }
}
