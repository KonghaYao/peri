use super::*;
use crate::kit::tui_render_unit::{TuiNoteLevel, TuiSystemNote};

fn snapshot(text: &str, content_hash: u64) -> ViewModelsSnapshot {
    ViewModelsSnapshot {
        generation: 1,
        items: vec![TuiRenderUnit::TuiSystemNote(TuiSystemNote {
            text: text.to_owned(),
            level: TuiNoteLevel::Error,
            content_hash,
        })]
        .into(),
    }
}

fn hit() -> CopyButtonHit {
    CopyButtonHit {
        row: 3,
        x_start: 1,
        x_end: 2,
        slot_index: 0,
        vm_hash: 17,
        logical_idx: 0,
    }
}

#[test]
fn error_click_copies_source_including_hidden_lines_and_whitespace() {
    let text = "  HTTP 400\n".to_owned() + &"完整响应 ".repeat(200) + "\nlast hidden line  ";
    assert_eq!(
        copy_text_for_hit(&snapshot(&text, 17), &hit()).as_deref(),
        Some(text.as_str())
    );
}

#[test]
fn stale_error_icon_cannot_copy_another_message_after_rewind() {
    assert!(copy_text_for_hit(&snapshot("replacement error", 18), &hit()).is_none());
}

#[test]
fn removed_error_icon_is_safe_after_reset() {
    let empty = ViewModelsSnapshot {
        generation: 2,
        items: im::Vector::new(),
    };
    assert!(copy_text_for_hit(&empty, &hit()).is_none());
}

#[test]
fn markdown_copy_keeps_stable_identity_instead_of_duration_hash() {
    let text = "full markdown ".repeat(50);
    let identity = TuiAssistantBubble::stable_identity_hash(&text, None);
    let source = ViewModelsSnapshot {
        generation: 1,
        items: vec![TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                text: text.clone(),
                reasoning: None,
                message_id: None,
                started_at: None,
                duration_ms: Some(2000),
                content_hash: 999,
            }
            .into(),
        )]
        .into(),
    };
    let mut button = hit();
    button.vm_hash = identity;
    assert_eq!(
        copy_text_for_hit(&source, &button).as_deref(),
        Some(text.as_str())
    );
}
