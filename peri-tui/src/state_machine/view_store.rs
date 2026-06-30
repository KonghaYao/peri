//! ViewStore -- the state machine's authoritative ViewModel list.
//!
//! Holds the last committed snapshot from the ACP layer's `"view-commit"` event.
//! Rendering concatenates this base list with the in-progress `CurrentTurn` to
//! produce the final view shown on screen.
//!
//! # Replacement semantics
//!
//! Per design doc section 4.2 + CLAUDE.md P2-C: `commit()` uses **replacement**
//! (not extend). The ACP layer sends a full ViewModel snapshot at each iteration
//! boundary; the old list is entirely discarded.
//!
//! Reference: `docs/design/peri-tui-architecture.md` sections 4.2, 4.4, 8.2.

use peri_acp_types::view_model::ViewModel;

use super::CurrentTurn;

/// The state machine's authoritative ViewModel list.
///
/// Updated on `"view-commit"` events via [`commit`](Self::commit). Rendering
/// accesses the combined list via [`for_render`](Self::for_render).
#[derive(Default, Debug, Clone)]
pub struct ViewStore {
    /// Last committed ViewModel snapshot from the ACP layer.
    ///
    /// Updated on every `"view-commit"` event. The list is replaced wholesale --
    /// never extended -- matching the design doc's "full replacement" invariant
    /// (section 4.2 + CLAUDE.md P2-C).
    pub view_models: Vec<ViewModel>,
}

impl ViewStore {
    /// Replace the entire list with a new view-commit snapshot.
    ///
    /// Per design section 4.2 + CLAUDE.md P2-C: **replacement** semantics (not
    /// extend). The `finalized_messages` from v2 stages are a full snapshot, not
    /// an incremental delta. Extending would cause the list to double on every
    /// commit in multi-iteration scenarios.
    pub fn commit(&mut self, view_models: Vec<ViewModel>) {
        self.view_models = view_models;
    }

    /// Clear the store (used on session switch).
    ///
    /// Called when entering `State::Switching` to discard the previous session's
    /// view data before the new session's first `"view-commit"` arrives.
    pub fn clear(&mut self) {
        self.view_models.clear();
    }

    /// Render-time accessor: base list + current turn appended.
    ///
    /// This is a pure function -- the caller passes `&CurrentTurn`. The output
    /// is a borrowed slice over the committed list, followed by borrowed
    /// references to the current turn's view models (if any).
    ///
    /// Rendering derives the final view as:
    /// ```text
    ///   view_models (committed) + current_turn.view_models() (in-progress)
    /// ```
    pub fn for_render<'a>(
        &'a self,
        current_turn: Option<&'a mut CurrentTurn>,
    ) -> Vec<&'a ViewModel> {
        let mut out: Vec<&ViewModel> = self.view_models.iter().collect();
        if let Some(ct) = current_turn {
            let ct_vms = ct.view_models();
            out.extend(ct_vms.iter());
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Free-function helpers (used by transitions/* without a ViewStore instance)
// ---------------------------------------------------------------------------

/// 在 ViewCommit 替换语义下保留前一轮的 TUI-only ViewModel（本地 SystemNote）。
///
/// v2 commit 是替换语义（`state.view = vc.view_models`），但 TUI 通过
/// `Event::PushSystemNote` 添加的 SystemNote **不在 ACP transcript 中**，
/// 纯替换会丢失它们。本函数：
/// 1. 收集 new_view 中已有的 SystemNote 文本（避免重复）
/// 2. 追加前一轮中不在 new_view 的 SystemNote
///
/// 只处理 SystemNote——其他 ViewModel 变体（UserBubble / AssistantBubble /
/// ToolCard / CacheWarning / ToolCallGroup / SubAgentGroup）由 ACP 层
/// view_mapper.rs 完整生成，不存在 TUI-only 的版本。
pub fn merge_preserving_local_notes(
    old_view: &[ViewModel],
    new_view: Vec<ViewModel>,
) -> Vec<ViewModel> {
    use std::collections::HashSet;

    let mut result = new_view;

    let existing_notes: HashSet<String> = result
        .iter()
        .filter_map(|vm| {
            if let ViewModel::SystemNote(d) = vm {
                Some(d.text.clone())
            } else {
                None
            }
        })
        .collect();

    for vm in old_view {
        if let ViewModel::SystemNote(d) = vm {
            if !existing_notes.contains(&d.text) {
                result.push(vm.clone());
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::view_model::{DividerData, UserBubbleData, ViewModel};

    #[test]
    fn test_default_is_empty() {
        let store = ViewStore::default();
        assert!(store.view_models.is_empty());
    }

    #[test]
    fn test_commit_replaces_wholesale() {
        let mut store = ViewStore::default();

        // First commit
        store.commit(vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })]);
        assert_eq!(store.view_models.len(), 1);

        // Second commit -- replaces, does not extend
        store.commit(vec![
            ViewModel::Divider(DividerData {
                label: Some("round 2".into()),
            }),
            ViewModel::UserBubble(UserBubbleData {
                text: "world".into(),
            }),
        ]);
        assert_eq!(store.view_models.len(), 2);
        // Old entry is gone -- replacement semantics
        match &store.view_models[0] {
            ViewModel::Divider(DividerData { label: Some(l) }) => {
                assert_eq!(l, "round 2");
            }
            _ => panic!("expected Divider variant"),
        }
    }

    #[test]
    fn test_clear() {
        let mut store = ViewStore::default();
        store.commit(vec![ViewModel::UserBubble(UserBubbleData {
            text: "data".into(),
        })]);
        assert!(!store.view_models.is_empty());

        store.clear();
        assert!(store.view_models.is_empty());
    }

    #[test]
    fn test_for_render_base_only() {
        let mut store = ViewStore::default();
        store.commit(vec![ViewModel::UserBubble(UserBubbleData {
            text: "committed".into(),
        })]);

        let rendered = store.for_render(None);
        assert_eq!(rendered.len(), 1);
    }

    #[test]
    fn test_for_render_with_current_turn() {
        let mut store = ViewStore::default();
        store.commit(vec![ViewModel::Divider(DividerData {
            label: Some("base".into()),
        })]);

        let mut current_turn = CurrentTurn::new();
        current_turn.append_text("streaming");

        let rendered = store.for_render(Some(&mut current_turn));
        assert_eq!(rendered.len(), 2);
        // First item is from committed base
        assert!(matches!(rendered[0], ViewModel::Divider(_)));
        // Second item is from current turn
        assert!(matches!(rendered[1], ViewModel::AssistantBubble(_)));
    }

    #[test]
    fn test_for_render_no_current_turn_no_panic() {
        let store = ViewStore::default();
        // None current_turn should not panic
        let rendered = store.for_render(None);
        assert!(rendered.is_empty());
    }

    // -- merge_preserving_local_notes (Phase 2.5) ----------------------------

    use super::merge_preserving_local_notes;
    use peri_acp_types::view_model::{NoteLevel, SystemNoteData};

    #[test]
    fn test_merge_preserves_tui_only_system_note() {
        // 旧 view 含 TUI-only SystemNote
        let old_view = vec![
            ViewModel::SystemNote(SystemNoteData {
                text: "/lang 切换到 en".into(),
                level: NoteLevel::Info,
            }),
            ViewModel::UserBubble(UserBubbleData {
                text: "hello".into(),
            }),
        ];
        // ACP 新快照只有 UserBubble（不含 SystemNote）
        let new_view = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let merged = merge_preserving_local_notes(&old_view, new_view);
        // UserBubble 来自 new_view，SystemNote 从 old_view 保留
        assert_eq!(merged.len(), 2);
        assert!(matches!(merged[0], ViewModel::UserBubble(_)));
        assert!(matches!(merged[1], ViewModel::SystemNote(_)));
    }

    #[test]
    fn test_merge_dedupes_system_notes_by_text() {
        // 旧 view 和新 view 都含相同文本的 SystemNote → 不应重复
        let old_view = vec![ViewModel::SystemNote(SystemNoteData {
            text: "same".into(),
            level: NoteLevel::Info,
        })];
        let new_view = vec![ViewModel::SystemNote(SystemNoteData {
            text: "same".into(),
            level: NoteLevel::Info,
        })];

        let merged = merge_preserving_local_notes(&old_view, new_view);
        assert_eq!(merged.len(), 1, "相同文本 SystemNote 不应重复");
    }

    #[test]
    fn test_merge_preserves_distinct_notes() {
        // 旧 view 有 2 条不同 SystemNote，新 view 有 1 条 → 合并后 3 条
        let old_view = vec![
            ViewModel::SystemNote(SystemNoteData {
                text: "old-1".into(),
                level: NoteLevel::Info,
            }),
            ViewModel::SystemNote(SystemNoteData {
                text: "old-2".into(),
                level: NoteLevel::Info,
            }),
        ];
        let new_view = vec![ViewModel::SystemNote(SystemNoteData {
            text: "new-1".into(),
            level: NoteLevel::Info,
        })];

        let merged = merge_preserving_local_notes(&old_view, new_view);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn test_merge_does_not_preserve_other_variants() {
        // 旧 view 的 UserBubble / AssistantBubble 等不应保留（ACP 完整生成）
        let old_view = vec![
            ViewModel::UserBubble(UserBubbleData {
                text: "stale".into(),
            }),
            ViewModel::Divider(DividerData { label: None }),
        ];
        let new_view = vec![ViewModel::UserBubble(UserBubbleData {
            text: "fresh".into(),
        })];

        let merged = merge_preserving_local_notes(&old_view, new_view);
        assert_eq!(merged.len(), 1, "非 SystemNote 不应保留");
        assert!(matches!(merged[0], ViewModel::UserBubble(_)));
    }
}
