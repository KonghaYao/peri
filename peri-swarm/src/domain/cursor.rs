use crate::domain::error::SwarmError;

/// Monotonic item cursor. `0` means "before the first item".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Cursor(u64);

impl Cursor {
    pub const START: Self = Self(0);

    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }

    /// Reconcile `afterSeq` (query), `after` (SDK query alias) and
    /// `Last-Event-ID`. Any disagreement is `400 conflicting_cursor`
    /// (`design §2.5`).
    pub fn reconcile(
        after_seq: Option<u64>,
        after: Option<u64>,
        last_event_id: Option<u64>,
    ) -> Result<Self, SwarmError> {
        let candidates = [after_seq, after, last_event_id];
        let mut resolved: Option<u64> = None;
        for candidate in candidates.into_iter().flatten() {
            match resolved {
                None => resolved = Some(candidate),
                Some(existing) if existing != candidate => {
                    return Err(SwarmError::conflicting_cursor());
                }
                Some(_) => {}
            }
        }
        Ok(Self(resolved.unwrap_or(0)))
    }

    /// Reconcile the two item-query spellings (`afterSeq` and the SDK `after`).
    /// Absence stays absence because `order=desc` treats it as "from newest".
    pub fn reconcile_optional(
        after_seq: Option<u64>,
        after: Option<u64>,
    ) -> Result<Option<u64>, SwarmError> {
        match (after_seq, after) {
            (Some(left), Some(right)) if left != right => Err(SwarmError::conflicting_cursor()),
            (Some(value), _) | (None, Some(value)) => Ok(Some(value)),
            (None, None) => Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "cursor_test.rs"]
mod tests;
