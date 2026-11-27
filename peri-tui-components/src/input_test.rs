use super::*;

#[test]
fn masking_and_caret_follow_the_visible_window() {
    let field = TextField::new("abcdef");
    let display = field.display(10, true);
    assert_eq!(display.text, "••••••");
    assert_eq!(display.caret_col, 6);

    let narrow = field.display(3, true);
    assert_eq!(narrow.text, "••");
    assert_eq!(narrow.caret_col, 2);

    let plain = field.display(3, false);
    assert_eq!(plain.text, "ef");
    assert_eq!(plain.caret_col, 2);
    // Masking is display-only: the stored value stays in clear text.
    assert_eq!(field.value(), "abcdef");
}

#[test]
fn clicking_maps_to_the_cell_under_the_cursor() {
    let mut field = TextField::new("abcdef");
    field.caret_from_column(0, 20, false);
    assert_eq!(field.caret(), 0);
    field.caret_from_column(2, 20, false);
    assert_eq!(field.caret(), 2);
    field.caret_from_column(99, 20, false);
    assert_eq!(field.caret(), 6);

    let mut masked = TextField::new("secret");
    masked.caret_from_column(3, 20, true);
    assert_eq!(masked.caret(), 3);
}

#[test]
fn editing_handles_unicode_and_pasted_controls() {
    let mut field = TextField::new("aé");
    assert_eq!(field.caret(), "aé".len());
    field.move_left();
    assert_eq!(field.caret(), 1);
    field.insert_str("ß\n\tb");
    assert_eq!(field.value(), "aßbé");
    assert_eq!(field.caret(), "aßb".len());
    field.move_home();
    field.delete_forward();
    assert_eq!(field.value(), "ßbé");
    field.backspace();
    assert_eq!(field.value(), "ßbé");
    field.move_end();
    field.backspace();
    assert_eq!(field.value(), "ßb");
}

#[test]
fn blank_values_are_trimmed_for_validation() {
    let mut field = TextField::new("  ");
    assert!(field.is_blank());
    field.insert_str(" x ");
    assert!(!field.is_blank());
    field.clear();
    assert!(field.is_blank());
}

#[test]
fn word_motion_uses_word_boundaries() {
    let mut field = TextField::new("hello world next");
    field.move_home();
    field.move_word_right();
    assert_eq!(field.caret(), 6);
    field.move_word_right();
    assert_eq!(field.caret(), 12);
    field.move_word_left();
    assert_eq!(field.caret(), 6);
    field.move_home();
    assert_eq!(field.caret(), 0);
}

#[test]
fn selection_extend_and_delete_selection_replace_text() {
    let mut field = TextField::new("hello world");
    field.move_home();
    field.extend_word_right();
    assert!(field.has_selection());
    assert_eq!(field.selection_range(), Some((0, 6)));
    assert_eq!(field.delete_selection().as_deref(), Some("hello "));
    assert_eq!(field.value(), "world");
    assert_eq!(field.caret(), 0);

    field.extend_end();
    assert_eq!(field.selection_range(), Some((0, 5)));
    field.insert_char('!');
    assert_eq!(field.value(), "!");
    assert!(!field.has_selection());
}

#[test]
fn select_all_and_backspace_clear_the_field() {
    let mut field = TextField::new("secret");
    field.select_all();
    assert_eq!(field.selection_range(), Some((0, 6)));
    field.backspace();
    assert_eq!(field.value(), "");
    assert_eq!(field.caret(), 0);
    assert!(!field.has_selection());
}

#[test]
fn undo_and_redo_walk_the_edit_history() {
    let mut field = TextField::new("");
    field.insert_str("ab");
    field.insert_char('c');
    assert_eq!(field.value(), "abc");
    assert!(field.undo());
    assert_eq!(field.value(), "ab");
    assert!(field.redo());
    assert_eq!(field.value(), "abc");
    assert!(!field.redo());
}

#[test]
fn debug_never_prints_the_value() {
    let field = TextField::new("sk-live-secret");
    let debug = format!("{field:?}");
    assert!(debug.contains("len"));
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("sk-live"));

    let mut cloned = field.clone();
    assert_eq!(cloned, field);
    cloned.insert_char('x');
    assert_ne!(cloned, field);
}

#[test]
fn selection_is_reported_in_visible_cells() {
    let mut field = TextField::new("abcdef");
    field.select_all();
    assert_eq!(field.display(10, false).selection, Some((0, 6)));
    assert_eq!(field.display(10, true).selection, Some((0, 6)));
    // The window is clipped to the last cells, so the selection is clipped too.
    assert_eq!(field.display(3, false).selection, Some((0, 2)));

    field.move_home();
    assert_eq!(field.display(10, false).selection, None);
    field.extend_right();
    field.extend_right();
    assert_eq!(field.display(10, false).selection, Some((0, 2)));
}

#[test]
fn word_delete_removes_the_word_around_the_caret() {
    let mut backward = TextField::new("alpha beta");
    backward.delete_word_left();
    assert_eq!(backward.value(), "alpha ");

    let mut forward = TextField::new("alpha beta");
    forward.move_home();
    forward.delete_word_right();
    assert_eq!(forward.value(), "beta");
}

#[test]
fn wide_characters_keep_the_caret_inside_the_window() {
    let mut field = TextField::new("你好");
    let display = field.display(5, false);
    assert_eq!(display.text, "你好");
    assert_eq!(display.caret_col, 4);

    let narrow = field.display(3, false);
    assert_eq!(narrow.text, "好");
    assert_eq!(narrow.caret_col, 2);

    field.caret_from_column(1, 3, false);
    assert_eq!(field.caret(), "你".len());
}
