//! Status projection regression scenarios.

#[test]
fn connected_at_parses_offsets_to_same_instant() {
    // %s（Unix 秒）不受本地时区影响：同一时刻的两种写法必须一致。
    let zulu = super::super::detail::format_connected_at("2026-10-11T06:32:05Z", "%s");
    let offset = super::super::detail::format_connected_at("2026-10-11T14:32:05+08:00", "%s");
    assert!(zulu.is_some());
    assert_eq!(zulu, offset);
}

#[test]
fn connected_at_rejects_invalid_timestamp() {
    assert_eq!(
        super::super::detail::format_connected_at("not-a-timestamp", "%H:%M"),
        None
    );
}

#[test]
fn cache_status_labels_are_explicit() {
    assert_eq!(
        super::super::detail::cache_status_label_key(Some("cache_disabled_by_config")),
        Some("panel-mcp-cache-disabled-config")
    );
    assert_eq!(
        super::super::detail::cache_status_label_key(Some("cache_pending")),
        Some("panel-mcp-cache-pending")
    );
    assert_eq!(
        super::super::detail::cache_status_label_key(Some("cache_ready")),
        Some("panel-mcp-cache-ready")
    );
    assert_eq!(
        super::super::detail::cache_status_label_key(Some("stored_after_fetch")),
        Some("panel-mcp-cache-saved")
    );
    assert_eq!(
        super::super::detail::cache_status_label_key(Some("cached")),
        Some("panel-mcp-cache-hit")
    );
    assert_eq!(
        super::super::detail::cache_status_label_key(Some("cache_disabled")),
        Some("panel-mcp-cache-disabled")
    );
    assert_eq!(
        super::super::detail::cache_status_label_key(Some("live_fetch")),
        Some("panel-mcp-cache-live-fetch")
    );
}
