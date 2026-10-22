//! cursor 完整性单元测试（设计 §12.3.4）。
//!
//! 只验证 MAC 编解码与服务层绑定，不依赖后端或窗口注册表。

use peri_acp_types::history_v2::HistoryDirection;
use peri_acp_types::history_v2::HistoryErrorReason;

use super::backend_test::{limits, page_params, snap};
use super::cursor::{decode, encode, CursorError};
use super::service_test::{call, open_ok, reason, setup};

fn secret(byte: u8) -> [u8; 32] {
    [byte; 32]
}

#[test]
fn cursor_roundtrip_preserves_raw_with_pipes() {
    let secret = secret(7);
    let cursor = encode(&secret, "w_1", HistoryDirection::Before, "raw|with|pipes");
    let decoded = decode(&secret, "w_1", &cursor).expect("valid cursor");
    assert_eq!(decoded.direction, HistoryDirection::Before);
    assert_eq!(decoded.raw, "raw|with|pipes");
}

#[test]
fn cursor_from_another_connection_secret_is_invalid() {
    // 两个连接对同一 windowId 使用不同 secret；跨连接复用必须是格式/MAC 失败，
    // 而不是把 cursor 当作可解释的位置提示。
    let encoded = encode(&secret(1), "w_1", HistoryDirection::After, "a0");
    assert!(matches!(
        decode(&secret(2), "w_1", &encoded),
        Err(CursorError::Invalid)
    ));
}

#[test]
fn tampered_raw_position_is_invalid() {
    let secret = secret(3);
    let cursor = encode(&secret, "w_1", HistoryDirection::Before, "b0");
    let tampered = cursor.replace("|b0", "|b1");
    assert!(matches!(
        decode(&secret, "w_1", &tampered),
        Err(CursorError::Invalid)
    ));
}

#[test]
fn tampered_window_id_is_invalid_before_cross_window_classification() {
    let secret = secret(4);
    let cursor = encode(&secret, "w_1", HistoryDirection::Before, "b0");
    let tampered = cursor.replace("|w_1|", "|w_2|");
    assert!(matches!(
        decode(&secret, "w_1", &tampered),
        Err(CursorError::Invalid)
    ));
}

#[test]
fn valid_cursor_for_another_window_is_cross_window() {
    let secret = secret(5);
    let cursor = encode(&secret, "w_2", HistoryDirection::Before, "b0");
    assert!(matches!(
        decode(&secret, "w_1", &cursor),
        Err(CursorError::CrossWindow)
    ));
}

#[test]
fn legacy_cursor_without_mac_is_invalid() {
    assert!(matches!(
        decode(&secret(6), "w_1", "v2|w_1|before|b0"),
        Err(CursorError::Invalid)
    ));
    assert!(matches!(
        decode(&secret(6), "w_1", "b0"),
        Err(CursorError::Invalid)
    ));
}

// ── 服务层 cursor 绑定（篡改与旧格式）────────────────────────────────────────

#[tokio::test]
async fn page_tampered_cursor_raw_is_invalid_cursor() {
    let (service, _clock, fake) = setup(snap(7));
    let (page, window) = open_ok(&service, &fake, 1).await;
    let cursor = page.before_cursor.expect("before cursor");
    let tampered = cursor.replacen("|b0", "|b1", 1);
    assert_ne!(tampered, cursor);

    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            Some(&tampered),
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::InvalidCursor);
    assert!(fake.page_calls.lock().is_empty());
}

#[tokio::test]
async fn page_tampered_cursor_window_id_is_invalid_cursor() {
    let (service, _clock, fake) = setup(snap(7));
    let (page, window) = open_ok(&service, &fake, 1).await;
    let cursor = page.before_cursor.expect("before cursor");
    let tampered = cursor.replacen(&format!("|{}|", window.id), "|w_other|", 1);
    assert_ne!(tampered, cursor);

    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            Some(&tampered),
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    // MAC 先于 windowId 校验：篡改身份不能被归类为 cross_window_cursor。
    assert_eq!(reason(&error), HistoryErrorReason::InvalidCursor);
    assert!(fake.page_calls.lock().is_empty());
}

#[tokio::test]
async fn page_legacy_cursor_without_mac_is_invalid_cursor() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    let legacy = format!("v2|{}|before|b0", window.id);
    let error = call(
        &service,
        &fake,
        limits(),
        page_params(
            2,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            Some(&legacy),
            "before",
            50,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::InvalidCursor);
    assert!(fake.page_calls.lock().is_empty());
}
