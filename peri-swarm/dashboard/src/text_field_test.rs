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
    field.move_left();
    field.insert_str("ß\n\tb");
    assert_eq!(field.value(), "aßbé");
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
