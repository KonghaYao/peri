use super::*;

#[test]
fn primary_buttons_fill_and_secondary_buttons_stay_plain() {
    assert_eq!(button_chrome(ButtonTone::Primary), ButtonChrome::Filled);
    assert_eq!(button_chrome(ButtonTone::Secondary), ButtonChrome::Plain);
    assert_eq!(button_chrome(ButtonTone::Danger), ButtonChrome::Plain);
    assert_eq!(button_width("Refresh", ButtonTone::Secondary), 7);
    assert_eq!(button_width("Create", ButtonTone::Primary), 8);
}

#[test]
fn primary_button_draws_a_filled_block_without_brackets() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));
    let rect = button_rect(Rect::new(0, 0, 12, 1), 0, "Go", ButtonTone::Primary);
    assert_eq!(rect.width, 4);
    render_button(&mut buf, rect, "Go", false, ButtonTone::Primary);
    let text: String = (0..rect.width).map(|x| buf[(x, 0)].symbol()).collect();
    assert_eq!(text, " Go ");
    assert!(buf[(0, 0)]
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
}

#[test]
fn secondary_button_draws_only_the_label() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));
    let rect = button_rect(Rect::new(0, 0, 12, 1), 0, "Go", ButtonTone::Secondary);
    assert_eq!(rect.width, 2);
    render_button(&mut buf, rect, "Go", false, ButtonTone::Secondary);
    let text: String = (0..rect.width).map(|x| buf[(x, 0)].symbol()).collect();
    assert_eq!(text, "Go");
    assert!(!buf[(0, 0)]
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
}

#[test]
fn word_wrap_prefers_spaces_and_hard_splits_long_words() {
    assert_eq!(wrap_words("", 10), vec![String::new()]);
    assert_eq!(wrap_words("one two", 20), vec!["one two"]);
    assert_eq!(wrap_words("one two three", 7), vec!["one two", "three"]);
    // A word wider than the column is split rather than truncated.
    assert_eq!(wrap_words("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    assert_eq!(
        wrap_words("save a file before anything is stored", 12),
        vec!["save a file", "before", "anything is", "stored"]
    );
    // Wide characters count as two columns.
    assert_eq!(wrap_words("会话 标识", 6), vec!["会话", "标识"]);
}
