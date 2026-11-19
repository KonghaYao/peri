use super::*;
use peri_theme::builtin::dark_theme;

#[test]
fn state_does_not_change_chrome_text_or_width() {
    let theme = dark_theme();
    for (chrome, text, width) in [
        (ButtonChrome::Filled, " 继续 ", 6),
        (ButtonChrome::Bracketed { padded: true }, "[ 继续 ]", 8),
        (ButtonChrome::Bracketed { padded: false }, "[继续]", 6),
    ] {
        for tone in [
            ButtonTone::Primary,
            ButtonTone::Secondary,
            ButtonTone::Danger,
        ] {
            for state in [
                ButtonState::Normal,
                ButtonState::Focused,
                ButtonState::Pressed,
                ButtonState::Disabled,
            ] {
                let visual = button_visual(
                    ButtonSpec {
                        label: "继续",
                        chrome,
                        tone,
                        state,
                    },
                    &theme,
                );
                assert_eq!((visual.text.as_str(), visual.width), (text, width));
            }
        }
    }
}

#[test]
fn styles_distinguish_states_using_theme_tokens() {
    let theme = dark_theme();
    let semantic = &theme.semantic;
    let style = |chrome, tone, state| button_style(chrome, tone, state, &theme);
    let filled = ButtonChrome::Filled;
    let bracketed = ButtonChrome::Bracketed { padded: true };
    let normal = style(filled, ButtonTone::Primary, ButtonState::Normal);
    assert_eq!(
        normal,
        Style::default()
            .fg(semantic.accent)
            .add_modifier(Modifier::REVERSED)
    );
    let pressed = style(filled, ButtonTone::Primary, ButtonState::Pressed);
    assert_ne!(normal, pressed);
    assert_eq!(
        (pressed.fg, pressed.bg),
        (normal.fg, Some(semantic.surface.default))
    );
    assert!(pressed.add_modifier.contains(Modifier::BOLD));
    assert!(pressed.sub_modifier.contains(Modifier::REVERSED));

    let normal = style(bracketed, ButtonTone::Primary, ButtonState::Normal);
    assert_eq!(normal, Style::default().fg(semantic.text.primary));
    let pressed = style(bracketed, ButtonTone::Primary, ButtonState::Pressed);
    assert_ne!(normal, pressed);
    assert_eq!(pressed.bg, Some(semantic.accent));
    assert_eq!(pressed.fg, Some(semantic.surface.default));
    assert!(pressed.add_modifier.contains(Modifier::BOLD));

    for chrome in [filled, bracketed] {
        let focused = style(chrome, ButtonTone::Secondary, ButtonState::Focused);
        let disabled = style(chrome, ButtonTone::Secondary, ButtonState::Disabled);
        assert_ne!(focused, disabled);
        assert_eq!(focused.fg, Some(semantic.accent));
        assert_eq!(disabled.fg, Some(semantic.text.muted));
        assert!(focused.add_modifier.contains(Modifier::BOLD));
        assert!(disabled.add_modifier.contains(Modifier::DIM));
    }
}

#[test]
fn hit_test_includes_first_column_but_not_end() {
    assert!(hit_test(4, 7, 4, 7, 9));
    assert!(hit_test(4, 8, 4, 7, 9));
    assert!(!hit_test(4, 6, 4, 7, 9));
    assert!(!hit_test(4, 9, 4, 7, 9));
    assert!(!hit_test(3, 7, 4, 7, 9));
    assert!(!hit_test(4, 7, 4, 7, 7));
    assert!(!hit_test(4, 7, 4, 9, 7));
}
