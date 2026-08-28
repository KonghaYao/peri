/// The portable compact action selected for the current context budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactAction {
    Skip,
    Micro,
    Smart,
}

/// Selects the native compact action without validating or clamping inputs.
pub fn select_compact_action(
    budget: f64,
    micro_threshold: f64,
    smart_enabled: bool,
) -> CompactAction {
    if budget >= micro_threshold {
        if smart_enabled {
            CompactAction::Smart
        } else {
            CompactAction::Micro
        }
    } else {
        CompactAction::Skip
    }
}
