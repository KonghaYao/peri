use super::*;
use crate::kit::atoms::{self, FocusedEntry};
use crate::kit::entry_render_cache::{EntryRenderContext, EntrySurface};
use crate::kit::markdown::GenUiSegment;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::transcript::Transcript;
use ratatui_kit::crossterm::event::{KeyModifiers, MouseEvent};
use unicode_width::UnicodeWidthStr;

struct Guard {
    focus: Option<FocusedEntry>,
    notification: Option<Notification>,
    restore: Option<String>,
    mention: bool,
    slash: bool,
    epoch: u64,
    history_cursor: peri_tui_components::history::HistoryCursor,
    active_panel: Option<crate::app::panel_types::PanelKind>,
    popup: Option<atoms::PopupKind>,
}

impl Guard {
    fn new() -> Self {
        atoms::init_atoms();
        crate::i18n::init(Some("en"));
        let guard = Self {
            focus: FOCUSED_ENTRY.state().read().clone(),
            notification: NOTIFICATION.state().write().take(),
            restore: INPUT_RESTORE_TEXT
                .get_or_init(Default::default)
                .lock()
                .take(),
            mention: AT_MENTION_ACTIVE.get(),
            slash: SLASH_HINT_ACTIVE.get(),
            epoch: BRIDGE_RESET_COUNTER.get(),
            history_cursor: atoms::INPUT_HISTORY_CURSOR.state().read().clone(),
            active_panel: atoms::ACTIVE_PANEL.state().write().take(),
            popup: atoms::POPUP_KIND.state().write().take(),
        };
        BRIDGE_RESET_COUNTER.set(guard.epoch.wrapping_add(1));
        guard
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        FOCUSED_ENTRY.set(self.focus.clone());
        NOTIFICATION.set(self.notification.take());
        *INPUT_RESTORE_TEXT.get_or_init(Default::default).lock() = self.restore.take();
        AT_MENTION_ACTIVE.set(self.mention);
        SLASH_HINT_ACTIVE.set(self.slash);
        BRIDGE_RESET_COUNTER.set(self.epoch);
        atoms::INPUT_HISTORY_CURSOR.set(self.history_cursor.clone());
        atoms::ACTIVE_PANEL.set(self.active_panel);
        atoms::POPUP_KIND.set(self.popup);
    }
}

fn segment(source: &str) -> GenUiSegment {
    GenUiSegment::parse(
        source.into(),
        false,
        peri_tui_components::genui::library::library().unwrap(),
    )
}

fn click(row: u16, column: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        row,
        column,
        modifiers: KeyModifiers::NONE,
    })
}

fn bubble(text: String) -> TuiRenderUnit {
    let mut bubble = TuiAssistantBubble {
        text,
        reasoning: None,
        message_id: None,
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    TuiRenderUnit::TuiAssistantBubble(Arc::new(bubble))
}

struct Fixture {
    snapshot: ViewModelsSnapshot,
    transcript: Transcript,
    caches: Vec<VmCacheSlot>,
    context: EntryRenderContext,
    grid: GridSpec,
}

impl Fixture {
    fn new(source: &str, width: u16) -> Self {
        Self {
            snapshot: ViewModelsSnapshot {
                generation: 1,
                items: vec![
                    bubble("preceding entry".into()),
                    bubble(format!("before\n\n```gen-ui\n{source}\n```\n\nafter")),
                ]
                .into(),
            },
            transcript: Transcript::default(),
            caches: Vec::new(),
            context: EntryRenderContext {
                theme: Arc::clone(&peri_theme::atoms::THEME_ATOM.state().read()),
                language: 0,
                surface: EntrySurface::Message,
                occurrence: BRIDGE_RESET_COUNTER.get(),
            },
            grid: GridSpec::grid_for(width),
        }
    }

    fn prepare(&mut self) -> Arc<SlotIndex> {
        self.transcript.prepare(
            &self.snapshot,
            &atoms::TranscriptPublication::default(),
            &mut self.caches,
            self.grid,
            self.grid.line_width(),
            self.context.clone(),
            0,
            BRIDGE_RESET_COUNTER.get(),
            0,
            100,
            0,
        )
    }

    fn text(&self) -> String {
        let lines = self.caches[1].lines.as_ref().unwrap();
        (0..lines.len())
            .map(|i| lines.line(i).unwrap().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[test]
#[serial_test::serial]
fn click_dispatch_accounts_for_entry_prefix_card_and_scrolled_viewport() {
    let _guard = Guard::new();
    let source = "root = Stack([Card([Button(\"继续中文\", @ToAssistant(\"下一步\"))], \"卡片\")])";
    for width in [40, 80, 120] {
        let mut fixture = Fixture::new(source, width);
        let index = fixture.prepare();
        let info = &fixture.caches[1].genui_buttons[0];
        let logical = info.line_start;
        let line = fixture.caches[1]
            .lines
            .as_ref()
            .unwrap()
            .line(logical)
            .unwrap()
            .to_string();
        let label_at = line.find("继续中文").unwrap();
        let prefix = line[..label_at].width() as u16 - 1;
        assert_eq!(info.x_start, prefix);
        assert_eq!(info.x_end - info.x_start, " 继续中文 ".width() as u16);
        let visual =
            index.slot_visual_start(1).unwrap() + fixture.caches[1].wrap_map[logical].visual_start;
        let area = Rect::new(9, 7, fixture.grid.line_width() + 1, 5);
        let hits = button_hits(
            &fixture.caches,
            &fixture.snapshot,
            area,
            3,
            visual - 1,
            &index,
        );
        let [hit] = hits.as_slice() else {
            panic!("应命中一个可见按钮")
        };
        assert_eq!(hit.row, area.y + 1);
        assert_eq!(hit.x_start, area.x + prefix);
        assert_eq!(
            dispatch_click(
                &click(hit.row, hit.x_start - 1),
                &hits,
                &fixture.snapshot,
                false,
                |key| assert!(key.is_none()),
            ),
            EventResult::Ignored
        );
        assert_eq!(
            dispatch_click(
                &click(hit.row, hit.x_end),
                &hits,
                &fixture.snapshot,
                false,
                |_| {}
            ),
            EventResult::Ignored
        );
        assert_eq!(
            dispatch_click(
                &click(hit.row, hit.x_start),
                &hits,
                &fixture.snapshot,
                true,
                |_| {}
            ),
            EventResult::Ignored
        );
        assert_eq!(
            dispatch_click(
                &click(hit.row, hit.x_start),
                &hits,
                &fixture.snapshot,
                false,
                |key| assert!(key.is_some_and(|key| key.matches(&hit.key))),
            ),
            EventResult::Consumed
        );
        assert_eq!(
            INPUT_RESTORE_TEXT.get().unwrap().lock().take().as_deref(),
            Some("下一步")
        );
        assert!(
            button_hits(
                &fixture.caches,
                &fixture.snapshot,
                area,
                1,
                visual + 1,
                &index
            )
            .is_empty()
        );
        assert!(
            button_hits(
                &fixture.caches,
                &fixture.snapshot,
                area,
                1,
                visual - 1,
                &index
            )
            .is_empty()
        );
        let empty = ViewModelsSnapshot::default();
        assert_eq!(
            dispatch_click(
                &click(hit.row, hit.x_start),
                &hits,
                &empty,
                false,
                |key| assert!(key.is_none())
            ),
            EventResult::Consumed
        );
        assert!(INPUT_RESTORE_TEXT.get().unwrap().lock().is_none());
        let mut replacement = fixture.snapshot.clone();
        replacement
            .items
            .set(1, bubble("replacement message".into()));
        assert_eq!(
            dispatch_click(
                &click(hit.row, hit.x_start),
                &hits,
                &replacement,
                false,
                |key| assert!(key.is_none())
            ),
            EventResult::Consumed
        );
        let epoch = BRIDGE_RESET_COUNTER.get();
        BRIDGE_RESET_COUNTER.set(epoch.wrapping_add(1));
        assert_eq!(
            dispatch_click(
                &click(hit.row, hit.x_start),
                &hits,
                &fixture.snapshot,
                false,
                |key| assert!(key.is_none()),
            ),
            EventResult::Consumed
        );
        BRIDGE_RESET_COUNTER.set(epoch);
        assert!(INPUT_RESTORE_TEXT.get().unwrap().lock().is_none());
    }
}

#[test]
#[serial_test::serial]
fn wrapped_button_hit_rows_match_paragraph_after_viewport_width_correction() {
    use ratatui_kit::ratatui::buffer::Buffer;
    use ratatui_kit::ratatui::widgets::{Paragraph, Widget, Wrap};

    let _guard = Guard::new();
    let mut fixture = Fixture::new(
        "root = Stack([Button(\"继续中文 continue with a long label\", @ToAssistant(\"next\"))])",
        80,
    );
    fixture.prepare();
    let slot = &fixture.caches[1];
    let button = &slot.genui_buttons[0];
    let line = slot
        .lines
        .as_ref()
        .unwrap()
        .line(button.line_start)
        .unwrap();
    let width = 16;
    let area = Rect::new(0, 0, width, 8);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .render(area, &mut buffer);
    let rows = super::super::artifact_hits::genui_button_rows(
        button,
        slot.lines.as_ref().unwrap().as_ref(),
        &slot.wrap_map,
        width,
    );
    assert!(rows.len() > 1);
    let visual_start = slot.wrap_map[button.line_start].visual_start;
    for (visual, start, end) in rows {
        let row = (visual - visual_start) as u16;
        assert!(start < end && end <= width);
        let text: String = (start..end)
            .map(|col| buffer[(col, row)].symbol())
            .collect();
        assert!(!text.trim().is_empty());
    }
}

#[test]
#[serial_test::serial]
fn pressed_overlay_covers_wrapped_hit_rows_without_changing_text() {
    use ratatui_kit::ratatui::widgets::{Paragraph, Widget, Wrap};

    let _guard = Guard::new();
    let source = "root = Stack([Button(\"继续中文 continue with a long label\", @ToAssistant(\"next\"), \"primary\")])";
    let mut fixture = Fixture::new(source, 80);
    let index = fixture.prepare();
    let slot = &fixture.caches[1];
    let logical = slot.genui_buttons[0].line_start;
    let scroll_y = index.slot_visual_start(1).unwrap() + slot.wrap_map[logical].visual_start;
    let area = Rect::new(3, 2, 17, 6);
    let hits = button_hits(
        &fixture.caches,
        &fixture.snapshot,
        area,
        area.height,
        scroll_y,
        &index,
    );
    assert!(hits.len() > 1);
    let key = hits.last().unwrap().key.clone();
    let line = slot.lines.as_ref().unwrap().line(logical).unwrap();
    let mut normal = Buffer::empty(area);
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .render(
            Rect {
                width: area.width - 1,
                ..area
            },
            &mut normal,
        );
    let mut pressed = normal.clone();
    draw_pressed(&mut pressed, &hits, &key, &fixture.context.theme);
    for hit in &hits {
        for col in hit.x_start..hit.x_end {
            let cell = &pressed[(col, hit.row)];
            assert!(cell.modifier.contains(Modifier::BOLD));
            assert!(!cell.modifier.contains(Modifier::REVERSED));
        }
        assert_eq!(pressed[(hit.x_end, hit.row)], normal[(hit.x_end, hit.row)]);
    }
    assert!(
        normal
            .content()
            .iter()
            .zip(pressed.content())
            .all(|(a, b)| a.symbol() == b.symbol())
    );

    // 同一按钮坐标与 VM 身份下，重建后的行仍然不得承接旧手势。
    let mut rebuilt = Fixture::new(source, 80);
    let index = rebuilt.prepare();
    let hits = button_hits(
        &rebuilt.caches,
        &rebuilt.snapshot,
        area,
        area.height,
        scroll_y,
        &index,
    );
    assert_eq!(hits.last().unwrap().key.vm_hash, key.vm_hash);
    assert!(!hits.last().unwrap().key.matches(&key));
    let mut after_rebuild = normal.clone();
    draw_pressed(&mut after_rebuild, &hits, &key, &fixture.context.theme);
    assert_eq!(after_rebuild, normal);
}

#[test]
fn press_cancels_on_up_wheel_leave_or_focus_loss_but_not_drag() {
    let outside = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            row: 0,
            column: 0,
            modifiers: KeyModifiers::NONE,
        })
    };
    let snapshot = ViewModelsSnapshot::default();
    for (event, should_clear) in [
        (outside(MouseEventKind::Drag(MouseButton::Left)), false),
        (outside(MouseEventKind::Up(MouseButton::Left)), true),
        (outside(MouseEventKind::ScrollDown), true),
        (outside(MouseEventKind::Moved), true),
        (Event::FocusLost, true),
    ] {
        let mut cleared = false;
        let result = dispatch_click(&event, &[], &snapshot, false, |key| {
            assert!(key.is_none());
            cleared = true;
        });
        assert_eq!(result, EventResult::Ignored);
        assert_eq!(cleared, should_clear, "{event:?}");
    }
}

#[test]
#[serial_test::serial]
fn streaming_blocks_reject_state_actions_until_closed() {
    let _guard = Guard::new();
    let source = r#"$count = 1
root = Stack([Text("stream=" + $count), Button("advance", @Set($count, $count + 1))])"#;
    let library = peri_tui_components::genui::library::library().unwrap();
    let theme = peri_theme::builtin::dark_theme();
    let partial = GenUiSegment::parse(source.into(), true, library);
    let rendered = crate::kit::genui::render(&partial, 80, &theme);
    assert!(rendered.buttons[0].partial);
    assert!(rendered.buttons[0].parsed.is_some());
    dispatch_action(&rendered.buttons[0]);
    assert_eq!(view_state::block_revision(partial.block_id()), 0);
    assert!(
        NOTIFICATION
            .state()
            .read()
            .as_ref()
            .is_some_and(|notification| notification.message.contains("state actions"))
    );

    let complete = GenUiSegment::parse(source.into(), false, library);
    let rendered = crate::kit::genui::render(&complete, 80, &theme);
    dispatch_action(&rendered.buttons[0]);
    assert!(view_state::block_revision(complete.block_id()) > 0);
    let rendered = crate::kit::genui::render(&complete, 80, &theme);
    assert!(
        rendered
            .lines
            .iter()
            .any(|line| line.to_string().contains("stream=2"))
    );
}

#[test]
#[serial_test::serial]
fn genui_state_updates_enforce_budget_per_slot() {
    let _guard = Guard::new();
    let text = r#"```gen-ui
$count = 1
root = Stack([Text("count=" + $count), Button("add", @Set($count, $count + 1))])
```"#;
    let items: im::Vector<TuiRenderUnit> = (0..4).map(|_| bubble(text.into())).collect();
    let mut fixture = Fixture {
        snapshot: ViewModelsSnapshot {
            generation: 1,
            items,
        },
        transcript: Transcript::default(),
        caches: Vec::new(),
        context: EntryRenderContext {
            theme: Arc::clone(&peri_theme::atoms::THEME_ATOM.state().read()),
            language: 0,
            surface: EntrySurface::Message,
            occurrence: BRIDGE_RESET_COUNTER.get(),
        },
        grid: GridSpec::grid_for(80),
    };
    fixture.prepare();
    let button = fixture.caches[0].genui_buttons[0].clone();
    dispatch_action(&button);
    fixture.prepare();
    assert_eq!(fixture.transcript.genui_budget_checks, 4);
    let (budget, _) = Transcript::render_cache_budgets();
    assert!(fixture.transcript.retained_bytes() <= budget);
}

#[test]
#[serial_test::serial]
fn set_and_reset_re_render_without_changing_transcript_or_other_blocks() {
    let _guard = Guard::new();
    let source = r#"$count = 1
$other = 9
root = Stack([Text("count=" + $count), Button("add", @Set($count, $count + 1)), Button("reset", @Reset($count, $other))])"#;
    let mut fixture = Fixture::new(source, 80);
    fixture.prepare();
    let original = fixture.snapshot.items.clone();
    let generation = fixture.snapshot.generation;
    let unchanged = Arc::clone(fixture.caches[0].lines.as_ref().unwrap());
    let first = fixture.caches[1].genui_buttons[0].clone();
    assert!(fixture.text().contains("count=1"));
    dispatch_action(&first);
    fixture.prepare();
    assert!(fixture.text().contains("count=2"));
    dispatch_action(&first);
    fixture.prepare();
    assert!(fixture.text().contains("count=3"));
    assert!(Arc::ptr_eq(
        &unchanged,
        fixture.caches[0].lines.as_ref().unwrap()
    ));
    assert_eq!(fixture.snapshot.generation, generation);
    assert_eq!(
        fixture.snapshot.items[1].content_hash(),
        original[1].content_hash()
    );
    assert_eq!(first.parsed.as_ref().unwrap().state["$count"], 1);
    let other = segment(&source.replace("count=", "different="));
    let independent = crate::kit::genui::render(&other, 70, &peri_theme::builtin::dark_theme());
    assert!(
        independent
            .lines
            .iter()
            .any(|line| line.to_string().contains("different=1"))
    );
    let reset = fixture.caches[1].genui_buttons[1].clone();
    dispatch_action(&reset);
    fixture.prepare();
    assert!(fixture.text().contains("count=1"));
    assert!(NOTIFICATION.state().read().is_none());
}

#[test]
#[serial_test::serial]
fn duplicate_blocks_share_view_state_but_reset_and_cold_render_remain_correct() {
    let _guard = Guard::new();
    let source = r#"$expanded = false
root = Stack([Button("toggle", @Set($expanded, !$expanded)), $expanded ? Text("expanded") : null])"#;
    let mut fixture = Fixture::new(source, 80);
    let before = fixture.prepare();
    let height = before.total_visual();
    let button = fixture.caches[1].genui_buttons[0].clone();
    fixture.caches[1].evict();
    dispatch_action(&button);
    let after = fixture.prepare();
    assert!(after.total_visual() > height);
    assert!(fixture.text().contains("expanded"));
    let duplicate = segment(source);
    assert_eq!(duplicate.block_id(), button.block.unwrap());
    let rendered = crate::kit::genui::render(&duplicate, 70, &peri_theme::builtin::dark_theme());
    assert!(
        rendered
            .lines
            .iter()
            .any(|line| line.to_string() == "expanded")
    );
    BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get().wrapping_add(1));
    fixture.prepare();
    assert!(!fixture.text().contains("expanded"));
}

#[test]
#[serial_test::serial]
fn invalid_state_action_is_visible_and_does_not_mutate_view() {
    let _guard = Guard::new();
    let segment = segment(
        "$count = 1\nroot = Stack([Text(\"count=\" + $count), Button(\"add\", @Set($count, 2))])",
    );
    let theme = peri_theme::builtin::dark_theme();
    let mut button = crate::kit::genui::render(&segment, 80, &theme).buttons[0].clone();
    button.action = GenUiAction::Set("$missing".into(), peri_genui::Expr::Null);
    dispatch_action(&button);
    assert!(
        NOTIFICATION
            .state()
            .read()
            .as_ref()
            .unwrap()
            .message
            .contains("$missing")
    );
    button.action = GenUiAction::Set(
        "$count".into(),
        peri_genui::Expr::RuntimeRef {
            n: "job".into(),
            ref_type: peri_genui::StatementKind::Query,
        },
    );
    dispatch_action(&button);
    assert!(
        NOTIFICATION
            .state()
            .read()
            .as_ref()
            .unwrap()
            .message
            .contains("Query")
    );
    let rendered = crate::kit::genui::render(&segment, 80, &theme);
    assert!(
        rendered
            .lines
            .iter()
            .any(|line| line.to_string() == "count=1")
    );
    assert_eq!(view_state::block_revision(segment.block_id()), 0);
}

#[test]
#[serial_test::serial]
fn unsupported_run_query_mutation_and_url_are_explicit() {
    let _guard = Guard::new();
    for kind in ["Query", "Mutation"] {
        let segment = segment(&format!(
            "job = {kind}(\"later\", {{}})\nroot = Stack([Button(\"run\", @Run(job))])"
        ));
        let rendered = crate::kit::genui::render(&segment, 80, &peri_theme::builtin::dark_theme());
        assert!(
            rendered
                .lines
                .iter()
                .any(|line| line.to_string().contains(kind))
        );
        assert_eq!(rendered.buttons.len(), 1);
        dispatch_action(&rendered.buttons[0]);
        let notification = NOTIFICATION
            .state()
            .read()
            .as_ref()
            .unwrap()
            .message
            .clone();
        assert!(
            notification.contains("@Run")
                && notification.contains("Query")
                && notification.contains("Mutation")
        );
    }
    let segment = segment("root = Stack([Button(\"url\", @OpenUrl(\"https://example.com\"))])");
    dispatch_action(
        &crate::kit::genui::render(&segment, 80, &peri_theme::builtin::dark_theme()).buttons[0],
    );
    assert!(
        NOTIFICATION
            .state()
            .read()
            .as_ref()
            .unwrap()
            .message
            .contains("@OpenUrl")
    );
}

#[tokio::test]
#[serial_test::serial]
async fn to_assistant_fills_real_composer_focuses_and_does_not_send() {
    let _guard = Guard::new();
    let saved_snapshot = atoms::INPUT_SNAPSHOT.state().read().clone();
    let before = atoms::VIEW_MODELS.state().read().generation;
    let queued = atoms::INPUT_BUFFER.state().read().len();
    let segment =
        segment("root = Stack([Button(\"按钮文本\", @ToAssistant(\"你好 👩‍💻\\n下一行\"))])");
    FOCUSED_ENTRY.set(Some(FocusedEntry { slot: 0, key: None }));
    let rendered = crate::kit::genui::render(&segment, 80, &peri_theme::builtin::dark_theme());
    dispatch_action(&rendered.buttons[0]);
    let _ = ratatui_kit::test_util::render_frames(
        element!(crate::kit::input_area::InputArea(grid: GridSpec::grid_for(80))),
        80,
        25,
        4,
    );
    let snapshot = atoms::INPUT_SNAPSHOT.state().read().clone();
    assert_eq!(snapshot.text, "你好 👩‍💻\n下一行");
    assert_eq!(snapshot.cursor_char, snapshot.text.chars().count());
    assert!(FOCUSED_ENTRY.state().read().is_none());
    assert!(crate::kit::focus_router::input_accepts_key(
        &ratatui_kit::crossterm::event::KeyEvent::new(
            ratatui_kit::crossterm::event::KeyCode::Enter,
            KeyModifiers::NONE,
        )
    ));
    assert!(INPUT_RESTORE_TEXT.get().unwrap().lock().is_none());
    assert_eq!(atoms::VIEW_MODELS.state().read().generation, before);
    assert_eq!(atoms::INPUT_BUFFER.state().read().len(), queued);
    let mut fallback = rendered.buttons[0].clone();
    fallback.action = GenUiAction::ToAssistant(String::new());
    dispatch_action(&fallback);
    assert_eq!(
        INPUT_RESTORE_TEXT.get().unwrap().lock().take().as_deref(),
        Some("按钮文本")
    );
    atoms::INPUT_SNAPSHOT.set(saved_snapshot);
}
