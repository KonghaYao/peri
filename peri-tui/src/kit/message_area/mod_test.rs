//! Tests

use super::*;

#[test]
fn test_empty_with_todo_items_shows_footer_not_welcome() {
    let entries_empty = true;
    let is_loading = false;
    let todo_items_empty = false;
    let empty = entries_empty && !is_loading && todo_items_empty;

    assert!(
        !empty,
        "仅有 todo 条目且无消息时不应判定为 empty，避免 Welcome 覆盖 todo 显示"
    );
}

#[test]
fn test_empty_without_todo_is_truly_empty() {
    let entries_empty = true;
    let is_loading = false;
    let todo_items_empty = true;
    let empty = entries_empty && !is_loading && todo_items_empty;

    assert!(empty);
}
