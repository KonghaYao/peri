//! Disposable display history has its own budget, without borrowing canonical credit.

use super::tui_render_unit::TuiRenderUnit;

pub(crate) const MAX_ENTRIES: usize = 256;
pub(crate) const MAX_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct DisplayBudget {
    entries: usize,
    bytes: usize,
}

impl DisplayBudget {
    /// Visit newest first. Keep the newest record readable even if it alone
    /// exceeds the budget; older records cannot accumulate behind it.
    pub(crate) fn retain(&mut self, bytes: usize) -> bool {
        if self.entries > 0
            && (self.entries >= MAX_ENTRIES || self.bytes.saturating_add(bytes) > MAX_BYTES)
        {
            return false;
        }
        self.entries += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        true
    }
}

pub(crate) fn is_disposable(unit: &TuiRenderUnit) -> bool {
    matches!(
        unit,
        TuiRenderUnit::TuiSystemNote(_)
            | TuiRenderUnit::TuiSystemReminder(_)
            | TuiRenderUnit::TuiAskUserBlock(_)
    )
}
