use crate::domain::error::SwarmError;

/// Starting point of an SSE subscription (`GET /events`).
///
/// A subscription that carries **no** cursor starts at the current tail and
/// delivers live events only: the official SDK subscribes before it submits the
/// input, and replaying an older turn would end its `TurnState` on the previous
/// turn's terminal. An explicit `afterSeq`/`after`/`Last-Event-ID` (including
/// `0`) replays the persisted backlog first (`design §4.2`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EventCursor {
    /// No cursor supplied: live events only, no backlog.
    #[default]
    Tail,
    /// Replay persisted items with `seq > inner`, then live events.
    After(Cursor),
}

impl EventCursor {
    /// Explicit `after=0`: replay the whole persisted log.
    pub const START: Self = Self::After(Cursor::START);

    #[must_use]
    pub fn after(seq: u64) -> Self {
        Self::After(Cursor::new(seq))
    }

    /// Replayed cursor, or `None` for a tail subscription.
    #[must_use]
    pub fn cursor(self) -> Option<Cursor> {
        match self {
            Self::Tail => None,
            Self::After(cursor) => Some(cursor),
        }
    }
}

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
    /// (`design §2.5`). No cursor at all keeps the [`EventCursor::Tail`]
    /// meaning instead of defaulting to `0`: absence is not "replay
    /// everything".
    pub fn reconcile_events(
        after_seq: Option<u64>,
        after: Option<u64>,
        last_event_id: Option<u64>,
    ) -> Result<EventCursor, SwarmError> {
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
        Ok(resolved.map_or(EventCursor::Tail, EventCursor::after))
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
