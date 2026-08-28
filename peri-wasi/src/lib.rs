wit_bindgen::generate!({
    world: "turn-policy",
    path: "wit",
});

use exports::peri::turn_policy::policy::{
    CompactAction, ContentClassification, ContentShape, Guest, PolicyError,
};
use peri_turn_policy::{
    is_message_content_empty, select_compact_action, CompactAction as SharedCompactAction,
    MessageContentShape,
};

struct Component;

impl Guest for Component {
    fn classify_content(content: ContentShape) -> ContentClassification {
        let shape = match &content {
            ContentShape::Text(text) => MessageContentShape::Text(text),
            ContentShape::Blocks(len) => MessageContentShape::Blocks(*len as usize),
            ContentShape::Raw(len) => MessageContentShape::Raw(*len as usize),
        };

        if is_message_content_empty(shape) {
            ContentClassification::Empty
        } else {
            ContentClassification::NonEmpty
        }
    }

    fn select_compact(budget: f64, micro_threshold: f64) -> Result<CompactAction, PolicyError> {
        if !budget.is_finite() {
            return Err(PolicyError::BudgetNotFinite);
        }
        if !(0.0..=1.0).contains(&budget) {
            return Err(PolicyError::BudgetOutOfRange);
        }
        if !micro_threshold.is_finite() {
            return Err(PolicyError::MicroThresholdNotFinite);
        }
        if !(0.0..=1.0).contains(&micro_threshold) {
            return Err(PolicyError::MicroThresholdOutOfRange);
        }

        match select_compact_action(budget, micro_threshold, false) {
            SharedCompactAction::Skip => Ok(CompactAction::Skip),
            SharedCompactAction::Micro => Ok(CompactAction::Micro),
            SharedCompactAction::Smart => unreachable!("Smart is disabled at the WIT boundary"),
        }
    }
}

export!(Component);
