use super::*;

#[test]
fn named_keys_carry_their_text_and_no_modifiers() {
    let enter = parse_key_combo("Enter").expect("Enter is known");
    assert_eq!(enter.key, "Enter");
    assert_eq!(enter.code, "Enter");
    assert_eq!(enter.text.as_deref(), Some("\r"));
    assert_eq!(enter.modifiers, 0);
    assert!(!enter.has_shortcut_modifier());

    let tab = parse_key_combo("Tab").unwrap();
    assert_eq!((tab.key.as_str(), tab.code.as_str()), ("Tab", "Tab"));
    assert_eq!(tab.text, None, "Tab must not insert a glyph");

    assert_eq!(parse_key_combo("Esc").unwrap().key, "Escape");
    assert_eq!(parse_key_combo("Down").unwrap().key, "ArrowDown");
    assert_eq!(parse_key_combo("PageUp").unwrap().key, "PageUp");
    assert_eq!(parse_key_combo("Space").unwrap().text.as_deref(), Some(" "));
}

#[test]
fn function_keys_cover_f1_to_f12_only() {
    assert_eq!(parse_key_combo("F1").unwrap().key, "F1");
    assert_eq!(parse_key_combo("f12").unwrap().code, "F12");
    assert!(parse_key_combo("F13").is_err());
    assert!(parse_key_combo("F0").is_err());
}

#[test]
fn chords_set_modifier_bits_and_suppress_text_for_shortcuts() {
    let ctrl_a = parse_key_combo("Control+A").unwrap();
    assert_eq!(ctrl_a.key, "a");
    assert_eq!(ctrl_a.code, "KeyA");
    assert_eq!(ctrl_a.text, None, "Ctrl+A must not type an 'a'");
    assert_eq!(ctrl_a.modifiers, MOD_CTRL);
    assert!(ctrl_a.has_shortcut_modifier());

    let shift_tab = parse_key_combo("Shift+Tab").unwrap();
    assert_eq!(
        (shift_tab.key.as_str(), shift_tab.code.as_str()),
        ("Tab", "Tab")
    );
    assert_eq!(shift_tab.modifiers, MOD_SHIFT);
    assert!(!shift_tab.has_shortcut_modifier());

    let cmd_shift_z = parse_key_combo("Cmd+Shift+Z").unwrap();
    assert_eq!(cmd_shift_z.key, "Z");
    assert_eq!(cmd_shift_z.code, "KeyZ");
    assert_eq!(cmd_shift_z.modifiers, MOD_META | MOD_SHIFT);
    assert_eq!(cmd_shift_z.text, None);

    let alt_enter = parse_key_combo("Alt+Enter").unwrap();
    assert_eq!(alt_enter.key, "Enter");
    assert_eq!(alt_enter.text, None);
    assert_eq!(alt_enter.modifiers, MOD_ALT);
}

#[test]
fn shifted_letters_keep_their_case_in_the_key_name() {
    let shifted = parse_key_combo("Shift+A").unwrap();
    assert_eq!(shifted.key, "A");
    assert_eq!(shifted.code, "KeyA");
    assert_eq!(shifted.text.as_deref(), Some("A"));

    let plain = parse_key_combo("A").unwrap();
    assert_eq!(plain.key, "a");
    assert_eq!(plain.text.as_deref(), Some("a"));
}

#[test]
fn malformed_chords_are_rejected_with_a_reason() {
    assert!(parse_key_combo("").unwrap_err().contains("empty"));
    assert!(parse_key_combo("Control+")
        .unwrap_err()
        .contains("missing key"));
    assert!(parse_key_combo("Hyper+A")
        .unwrap_err()
        .contains("unknown modifier"));
    assert!(parse_key_combo("Frobnicate")
        .unwrap_err()
        .contains("unknown key"));
    assert!(parse_key_combo("ab").unwrap_err().contains("unknown key"));
}
