use super::*;
use peri_theme::builtin::dark_theme;

#[test]
fn state_does_not_change_chrome_text_or_width() {
    let theme = dark_theme();
    for (chrome, text, width) in [
        (ButtonChrome::Filled, " 继续 ", 6),
        (ButtonChrome::Plain, "继续", 4),
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

    let plain = ButtonChrome::Plain;
    let normal = style(plain, ButtonTone::Primary, ButtonState::Normal);
    assert_eq!(normal, Style::default().fg(semantic.text.primary));
    let pressed = style(plain, ButtonTone::Primary, ButtonState::Pressed);
    assert_eq!(pressed.bg, Some(semantic.accent));
    assert_eq!(pressed.fg, Some(semantic.surface.default));

    for chrome in [filled, plain, bracketed] {
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

fn row_text(buf: &Buffer, x: u16, y: u16, width: u16) -> String {
    (x..x.saturating_add(width))
        .map(|col| buf.cell((col, y)).map_or("", ratatui::buffer::Cell::symbol))
        .collect()
}

#[test]
fn button_width_matches_button_visual_for_every_state() {
    let theme = dark_theme();
    for chrome in [
        ButtonChrome::Filled,
        ButtonChrome::Plain,
        ButtonChrome::Bracketed { padded: true },
        ButtonChrome::Bracketed { padded: false },
    ] {
        for state in [
            ButtonState::Normal,
            ButtonState::Focused,
            ButtonState::Pressed,
            ButtonState::Disabled,
        ] {
            let button = Button::new("继续", chrome, ButtonTone::Primary, state);
            let visual = button_visual(
                ButtonSpec {
                    label: "继续",
                    chrome,
                    tone: ButtonTone::Primary,
                    state,
                },
                &theme,
            );
            assert_eq!(button.width(), visual.width);
        }
    }
}

#[test]
fn button_render_draws_chrome_at_rect_origin_with_the_state_style() {
    let theme = dark_theme();
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
    let button = Button::new(
        "Ok",
        ButtonChrome::Bracketed { padded: true },
        ButtonTone::Secondary,
        ButtonState::Focused,
    );
    let rect = Rect::new(2, 1, 10, 1);
    button.render(&mut buf, rect, &theme);

    assert_eq!(row_text(&buf, 2, 1, 6), "[ Ok ]");
    assert_eq!(buf[(1, 1)].symbol(), " ");
    assert_eq!(buf[(8, 1)].symbol(), " ");
    let expected = button_style(
        ButtonChrome::Bracketed { padded: true },
        ButtonTone::Secondary,
        ButtonState::Focused,
        &theme,
    );
    for col in 2..8 {
        let style = buf[(col, 1)].style();
        assert_eq!(style.fg, expected.fg);
        assert_eq!(style.add_modifier, expected.add_modifier);
    }
}

#[test]
fn plain_chrome_draws_only_the_label() {
    let theme = dark_theme();
    let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
    let button = Button::new(
        "Ok",
        ButtonChrome::Plain,
        ButtonTone::Secondary,
        ButtonState::Normal,
    );
    assert_eq!(button.width(), 2);
    button.render(&mut buf, Rect::new(1, 0, 6, 1), &theme);

    assert_eq!(row_text(&buf, 1, 0, 2), "Ok");
    assert_eq!(buf[(0, 0)].symbol(), " ");
    assert_eq!(buf[(3, 0)].symbol(), " ");
}

#[test]
fn button_render_disabled_uses_dim_muted_style() {
    let theme = dark_theme();
    let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));
    Button::new(
        "Quit",
        ButtonChrome::Bracketed { padded: true },
        ButtonTone::Danger,
        ButtonState::Disabled,
    )
    .render(&mut buf, Rect::new(0, 0, 12, 1), &theme);

    let style = buf[(0, 0)].style();
    assert_eq!(style.fg, Some(theme.semantic.text.muted));
    assert!(style.add_modifier.contains(Modifier::DIM));
    assert!(!style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn button_render_truncates_to_rect_width() {
    let theme = dark_theme();
    let mut buf = Buffer::empty(Rect::new(0, 0, 12, 1));
    Button::new(
        "Cancel",
        ButtonChrome::Bracketed { padded: true },
        ButtonTone::Primary,
        ButtonState::Normal,
    )
    .render(&mut buf, Rect::new(0, 0, 5, 1), &theme);

    assert_eq!(row_text(&buf, 0, 0, 5), "[ Ca…");
    assert_eq!(buf[(5, 0)].symbol(), " ");
}

#[test]
fn button_render_empty_rect_is_a_no_op() {
    let theme = dark_theme();
    let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
    let button = Button::new(
        "Ok",
        ButtonChrome::Filled,
        ButtonTone::Primary,
        ButtonState::Normal,
    );
    button.render(&mut buf, Rect::new(0, 0, 0, 0), &theme);
    button.render(&mut buf, Rect::new(3, 0, 0, 1), &theme);
    button.render(&mut buf, Rect::new(0, 5, 4, 1), &theme);
    for row in 0..2 {
        for col in 0..4 {
            assert_eq!(buf[(col, row)].symbol(), " ");
        }
    }
}
