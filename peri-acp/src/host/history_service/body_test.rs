//! History V2 正文授权与窗口拆除（H2）。
//!
//! `bodyRef` 不是 MAC cursor：服务只接受窗口实际交付过的 opaque 引用；适配器在
//! 读取正文前再按 role/前缀受众复核一次。用例使用 `FakeBackend`，不读 SQLite。

use peri_acp_types::history_v2::{
    DecimalU64, HistoryBodyChunk, HistoryBodyDescriptor, HistoryDurability, HistoryDurabilityState,
    HistoryEntry, HistoryErrorReason, HistoryOpaqueSubItem, HistoryOrder, HistoryOrigin,
    HistoryPageData, HistoryRepresentation, HistorySource, HistorySubItem,
};

use super::backend_test::{
    body_params, body_slice, close_params, limits, open_params, page_data, page_params, snap,
};
use super::service_test::{call, open_ok, parse_page, reason, setup, window_of, Window};

/// 一页包含单个 opaque 条目；`bodyRef` 是该条目交付给客户端的唯一读取句柄。
fn opaque_page(body_ref: &str) -> HistoryPageData {
    let mut page = page_data(7, Some("b0"), Some("a0"));
    page.entries = vec![HistoryEntry {
        entry_id: "e0".to_owned(),
        source: HistorySource::Own,
        content_version: "v1".to_owned(),
        durability: HistoryDurability {
            state: HistoryDurabilityState::Sealed,
            origin: HistoryOrigin::Imported,
            final_item_seq: None,
        },
        order: HistoryOrder {
            region: "own".to_owned(),
            ordinal: DecimalU64::new(0),
        },
        sub_items: vec![HistorySubItem::Opaque(HistoryOpaqueSubItem {
            sub_item_id: "e0:0".to_owned(),
            body: HistoryBodyDescriptor {
                body_ref: body_ref.to_owned(),
                representation: HistoryRepresentation::DisplayJson,
                media_type: "application/json".to_owned(),
                total_bytes: DecimalU64::new(5),
                chunk_bytes: 64 * 1024,
            },
        })],
    }];
    page
}

fn snap_with_opaque(body_ref: &str) -> peri_acp_types::history_v2::HistoryOpenedSnapshot {
    let mut opened = snap(7);
    opened.page = opaque_page(body_ref);
    opened
}

#[tokio::test]
async fn body_returns_chunk_and_close_rejects_later_body() {
    let (service, _clock, fake) = setup(snap_with_opaque("body-1"));
    let (_page, window) = open_ok(&service, &fake, 1).await;
    fake.set_body_result(Ok(body_slice()));
    let value = call(
        &service,
        &fake,
        limits(),
        body_params(2, "s1", &window.id, &window.token, &window.ticket),
    )
    .await
    .expect("body succeeds");
    let chunk: HistoryBodyChunk = serde_json::from_value(value).expect("body chunk");
    assert_eq!(chunk.offset.get(), 0);
    assert_eq!(chunk.data, "aGVsbG8=");
    assert!(chunk.complete);
    {
        let calls = fake.body_calls.lock();
        assert_eq!(calls[0].body_ref, "body-1");
        assert_eq!(calls[0].offset, 0);
        assert_eq!(calls[0].max_bytes, 65536);
    }

    call(
        &service,
        &fake,
        limits(),
        close_params(3, "s1", &window.id, &window.token, &window.ticket),
    )
    .await
    .expect("close");
    let error = call(
        &service,
        &fake,
        limits(),
        body_params(4, "s1", &window.id, &window.token, &window.ticket),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::WindowClosed);
}

#[tokio::test]
async fn body_ref_must_be_issued_by_the_window() {
    let (service, _clock, fake) = setup(snap(7));
    let (_page, window) = open_ok(&service, &fake, 1).await;

    // 窗口没有交付过任何 opaque 条目：猜到的 bodyRef 不能读取。
    let error = call(
        &service,
        &fake,
        limits(),
        body_params(2, "s1", &window.id, &window.token, &window.ticket),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), HistoryErrorReason::NotFound);
    assert!(fake.body_calls.lock().is_empty(), "未授权不得触达后端");

    // 同一窗口交付 opaque 条目后，同一 bodyRef 才可读取。
    fake.set_page_result(Ok(opaque_page("body-1")));
    call(
        &service,
        &fake,
        limits(),
        page_params(
            3,
            "s1",
            &window.id,
            &window.token,
            &window.ticket,
            None,
            "before",
            50,
        ),
    )
    .await
    .expect("page with opaque entry");
    fake.set_body_result(Ok(body_slice()));
    call(
        &service,
        &fake,
        limits(),
        body_params(4, "s1", &window.id, &window.token, &window.ticket),
    )
    .await
    .expect("delivered bodyRef succeeds");
    assert_eq!(fake.body_calls.lock().len(), 1);
}

#[tokio::test]
async fn close_session_closes_only_that_sessions_windows() {
    let (service, _clock, fake) = setup(snap_with_opaque("body-1"));
    let (_s1_page, s1): (_, Window) = open_ok(&service, &fake, 1).await;

    fake.set_open_result(Ok(snap_with_opaque("body-1")));
    let s2_value = call(&service, &fake, limits(), open_params(2, "s2", 50, 0))
        .await
        .expect("open s2");
    let s2 = window_of(&parse_page(&s2_value));

    service.close_session("s1");
    assert_eq!(
        service.window_count(),
        1,
        "other sessions keep their windows"
    );

    let page_error = call(
        &service,
        &fake,
        limits(),
        page_params(3, "s1", &s1.id, &s1.token, &s1.ticket, None, "before", 50),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&page_error), HistoryErrorReason::WindowClosed);

    let body_error = call(
        &service,
        &fake,
        limits(),
        body_params(4, "s1", &s1.id, &s1.token, &s1.ticket),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&body_error), HistoryErrorReason::WindowClosed);

    // s2 窗口仍可继续翻页。
    fake.set_page_result(Ok(page_data(7, Some("b0"), Some("a0"))));
    call(
        &service,
        &fake,
        limits(),
        page_params(5, "s2", &s2.id, &s2.token, &s2.ticket, None, "before", 50),
    )
    .await
    .expect("s2 page remains usable");
}
