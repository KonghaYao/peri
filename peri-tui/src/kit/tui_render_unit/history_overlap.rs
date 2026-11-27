//! Locate a contiguous run whose per-entry render inputs survived a history page merge.
use super::TuiRenderUnit;
use im::Vector;

#[derive(Clone, Copy, Default)]
pub(crate) struct HistoryOverlap {
    pub(crate) old_start: usize,
    pub(crate) new_start: usize,
    pub(crate) len: usize,
}

pub(crate) fn same_render_unit(a: &TuiRenderUnit, b: &TuiRenderUnit) -> bool {
    use TuiRenderUnit::*;
    if a.content_hash() != b.content_hash() {
        return false;
    }
    match (a, b) {
        (TuiUserBubble(a), TuiUserBubble(b)) => {
            a.message_id == b.message_id && a.source == b.source
        }
        (TuiAssistantBubble(a), TuiAssistantBubble(b)) => a.message_id == b.message_id,
        (TuiToolCard(a), TuiToolCard(b)) => a.tool_id == b.tool_id,
        (TuiSystemReminder(a), TuiSystemReminder(b)) => a.reminder_id == b.reminder_id,
        (TuiSubAgentGroup(a), TuiSubAgentGroup(b)) => a.instance_id == b.instance_id,
        (TuiCollapsedGroup(a), TuiCollapsedGroup(b)) => a.key == b.key,
        (TuiAskUserBlock(a), TuiAskUserBlock(b)) => a.request_id == b.request_id,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

/// Compare shared im chunks by address. Only the seams of a spliced tree need entry comparisons.
fn matching_run(
    old: &Vector<TuiRenderUnit>,
    new: &Vector<TuiRenderUnit>,
    mut i: usize,
    mut j: usize,
    backwards: bool,
) -> usize {
    let mut a = old.focus();
    let mut b = new.focus();
    let mut matched = 0;
    loop {
        let (x, y) = if backwards {
            if i == 0 || j == 0 {
                break;
            }
            (i - 1, j - 1)
        } else {
            if i == old.len() || j == new.len() {
                break;
            }
            (i, j)
        };
        let (ar, ac) = a.chunk_at(x);
        let (br, bc) = b.chunk_at(y);
        let (al, bl, n) = if backwards {
            let n = (i - ar.start).min(j - br.start);
            (i - ar.start - n, j - br.start - n, n)
        } else {
            (i - ar.start, j - br.start, (ar.end - i).min(br.end - j))
        };
        let ac = &ac[al..al + n];
        let bc = &bc[bl..bl + n];
        if !std::ptr::eq(ac, bc) {
            let mut pairs = ac.iter().zip(bc);
            let diff = if backwards {
                pairs.rev().position(|(a, b)| !same_render_unit(a, b))
            } else {
                pairs.position(|(a, b)| !same_render_unit(a, b))
            };
            if let Some(offset) = diff {
                return matched + offset;
            }
        }
        matched += n;
        if backwards {
            i -= n;
            j -= n;
        } else {
            i += n;
            j += n;
        }
    }
    matched
}

/// Keep the interior chunks of a large suffix shared. im::Vector::append rebalances both
/// trees and can copy the large operand's leaves even for a very small prefix.
pub(crate) fn prepend_units(
    mut prefix: Vector<TuiRenderUnit>,
    mut suffix: Vector<TuiRenderUnit>,
) -> Vector<TuiRenderUnit> {
    if prefix.len() <= suffix.len() {
        for unit in prefix.into_iter().rev() {
            suffix.push_front(unit);
        }
        suffix
    } else {
        prefix.append(suffix);
        prefix
    }
}

/// When the original vector is an intact suffix, only this many leading items were added.
pub(crate) fn prepended_len(
    old: &Vector<TuiRenderUnit>,
    new: &Vector<TuiRenderUnit>,
) -> Option<usize> {
    if old.is_empty() || new.len() <= old.len() {
        return None;
    }
    let added = new.len() - old.len();
    if !same_render_unit(&old[0], &new[added]) {
        return None;
    }
    (matching_run(old, new, 0, added, false) == old.len()).then_some(added)
}

pub(crate) fn history_overlap(
    old: &Vector<TuiRenderUnit>,
    new: &Vector<TuiRenderUnit>,
) -> HistoryOverlap {
    let len = matching_run(old, new, old.len(), new.len(), true);
    let suffix = HistoryOverlap {
        old_start: old.len() - len,
        new_start: new.len() - len,
        len,
    };
    // The first old group can be merged into the incoming page. Everything after it is stable.
    if suffix.old_start <= 1 || suffix.new_start == 0 {
        return suffix;
    }
    // A release or live tail update can also remove the common suffix. Locate the leading old
    // body, then verify the complete contiguous run. Repeated messages are safe to reuse only
    // for the actual length that compares equal, including their render/fold identities.
    let old_start = old
        .iter()
        .take(3)
        .position(|unit| {
            !matches!(
                unit,
                TuiRenderUnit::TuiDivider(_) | TuiRenderUnit::TuiCollapsedGroup(_)
            )
        })
        .unwrap_or(0);
    let Some(first) = old.get(old_start) else {
        return suffix;
    };
    let Some(new_start) = new.iter().position(|unit| same_render_unit(first, unit)) else {
        return suffix;
    };
    let right = matching_run(old, new, old_start, new_start, false);
    let left = matching_run(old, new, old_start, new_start, true);
    if left + right > len {
        HistoryOverlap {
            old_start: old_start - left,
            new_start: new_start - left,
            len: left + right,
        }
    } else {
        suffix
    }
}
