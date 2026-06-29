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
}
