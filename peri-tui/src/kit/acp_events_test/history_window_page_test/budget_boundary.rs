use super::*;

use crate::kit::atoms;
use crate::kit::history_page_consumer::{WindowClaim, WindowSettlement};
use crate::kit::history_paging::WindowPageCursor;
use crate::kit::history_window::{
    PersistentEntryKey, WindowAnchor, WindowAnchorOffset, WindowLimits, WindowViewport,
};

struct RenderAckGuard(u64);

impl Drop for RenderAckGuard {
    fn drop(&mut self) {
        atoms::RENDER_ACHIEVED_EPOCH.set(self.0);
    }
}

fn rows(first: u64) -> Vec<WindowPageItem> {
    (first..first + 2)
        .map(|n| text_item(&format!("m{n:02}"), n, &format!("row{n:02}")))
        .collect()
}

fn read_page(state: &mut BridgeState, cursor: WindowPageCursor, first: u64) -> PublicationIntent {
    let paging = atoms::HISTORY_WINDOW_PAGING.state();
    let previous = paging.read().clone();
    match &cursor {
        WindowPageCursor::Before(edge) => assert_eq!(previous.before_cursor.as_ref(), Some(edge)),
        WindowPageCursor::After(edge) => assert_eq!(previous.after_cursor.as_ref(), Some(edge)),
    }
    let request = history_page_consumer::build_window_page_request(cursor, 2).expect("page");
    let WindowClaim::Granted(claim) = history_page_consumer::claim_window_request(&request) else {
        panic!("the previous page must have settled");
    };
    let mut page = response(claim.context.identity.clone(), rows(first));
    page.before_cursor = Some(format!("before-{first:02}"));
    page.after_cursor = Some(format!("after-{:02}", first + 1));
    page.read_ticket = format!("ticket-{first:02}");
    assert!(matches!(
        history_page_consumer::settle_window_response(&claim, &page),
        WindowSettlement::Accepted(_)
    ));
    let intent = super::super::dispatch_for_bridge(
        state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(claim.context),
            response: Box::new(page),
        },
    );
    if intent == PublicationIntent::Deferred {
        let pending = paging.read();
        assert_eq!(pending.before_cursor, previous.before_cursor);
        assert_eq!(pending.after_cursor, previous.after_cursor);
        assert_eq!(pending.read_ticket, previous.read_ticket);
        drop(pending);
        super::super::push_view_models(state);
    }
    intent
}

#[test]
#[serial]
fn acknowledged_budget_release_keeps_both_directions_contiguous_and_retries_the_same_page() {
    let (mut state, _guard) = state_with_window();
    crate::kit::history_window::clear_window_viewport();
    let _ack = RenderAckGuard(atoms::RENDER_ACHIEVED_EPOCH.get());
    atoms::RENDER_ACHIEVED_EPOCH.set(0);
    let page_bytes = rows(10)
        .iter()
        .map(|item| serde_json::to_vec(&item.sub_items[0].update).unwrap().len())
        .sum::<usize>();
    let identity = state.window.identity().unwrap().clone();
    state.window = HistoryWindow::new(HistoryWindowConfig {
        history_window_v1: true,
        limits: WindowLimits {
            body_bytes: 3 * page_bytes,
            ..WindowLimits::default()
        },
    });
    state.window.begin_window(identity);
    let paging = atoms::HISTORY_WINDOW_PAGING.state();
    paging.write().before_cursor = Some("before-12".into());

    let before = |n| WindowPageCursor::Before(format!("before-{n:02}"));
    let after = |n| WindowPageCursor::After(format!("after-{n:02}"));
    let expected = |first, last| {
        (first..=last)
            .map(|n| format!("row{n:02}"))
            .collect::<Vec<_>>()
    };
    for first in [10, 8, 6, 4] {
        assert_eq!(
            read_page(&mut state, before(first + 2), first),
            PublicationIntent::Deferred
        );
    }
    assert!(state.window.pending_release_bytes() > 0);
    assert_eq!(committed_texts(&state), expected(4, 7));
    let evicted = state.window.stats().evicted_segments;
    // `dispatch_for_bridge` schedules a publication for every page event even when the
    // handler rejects it; the rejection itself must leave the window untouched.
    assert_eq!(
        read_page(&mut state, before(4), 2),
        PublicationIntent::Deferred
    );
    assert_eq!(committed_texts(&state), expected(4, 7));
    assert_eq!(state.window.stats().evicted_segments, evicted);
    assert_eq!(paging.read().before_cursor.as_deref(), Some("before-04"));
    assert_eq!(paging.read().read_ticket.as_deref(), Some("ticket-04"));
    assert_eq!(paging.read().reserved_bytes, 0);

    // The renderer drew the eviction frame, but no intervening bridge publication occurred.
    atoms::RENDER_ACHIEVED_EPOCH.set(state.window.pending_release_epoch());
    assert_eq!(
        read_page(&mut state, before(4), 2),
        PublicationIntent::Deferred
    );
    assert_eq!(committed_texts(&state), expected(2, 7));
    assert_eq!(paging.read().after_cursor.as_deref(), Some("after-07"));

    // Reverse the wheel at the newer edge and read both evicted pages back in order.
    atoms::WINDOW_VIEWPORT.set(Some(WindowViewport {
        anchor: Some(WindowAnchor::new(
            PersistentEntryKey::own("s1:m07"),
            None,
            WindowAnchorOffset::CharOffset(0),
        )),
        ..WindowViewport::default()
    }));
    assert_eq!(
        read_page(&mut state, after(7), 8),
        PublicationIntent::Deferred
    );
    assert_eq!(committed_texts(&state), expected(6, 9));
    atoms::RENDER_ACHIEVED_EPOCH.set(state.window.pending_release_epoch());
    assert_eq!(
        read_page(&mut state, after(9), 10),
        PublicationIntent::Deferred
    );
    assert_eq!(committed_texts(&state), expected(6, 11));
    // Every entry was already displayed, so the page collapses to an empty draft; the
    // page event still schedules a publication but the window must not move.
    assert_eq!(
        read_page(&mut state, after(11), 10),
        PublicationIntent::Deferred
    );
    assert_eq!(committed_texts(&state), expected(6, 11));
    assert_eq!(paging.read().after_cursor.as_deref(), Some("after-11"));
}
