//! Session-local manual folds have a separate budget from canonical history.

use std::collections::{HashMap, VecDeque};
use std::mem::size_of;

use parking_lot::Mutex;

use super::atoms::FOLD_OVERRIDES;
use super::tui_render_unit::{FoldKey, FoldState};

const MAX_ENTRIES: usize = 1024;
const MAX_KEY_BYTES: usize = 256 * 1024;

static RETENTION: Mutex<Retention> = Mutex::new(Retention {
    order: VecDeque::new(),
});

#[derive(Default)]
struct Retention {
    order: VecDeque<FoldKey>,
}

fn key_bytes(key: &FoldKey) -> usize {
    size_of::<FoldKey>()
        + match key {
            FoldKey::Reasoning(id)
            | FoldKey::Tool(id)
            | FoldKey::SubAgent(id)
            | FoldKey::Interaction(id) => id.capacity(),
            FoldKey::Group(ids) => {
                ids.capacity() * size_of::<String>()
                    + ids.iter().map(String::capacity).sum::<usize>()
            }
            FoldKey::SystemReminder(_) => 0,
        }
}

impl Retention {
    fn insert(&mut self, values: &mut HashMap<FoldKey, FoldState>, key: FoldKey, fold: FoldState) {
        self.order
            .retain(|old| old != &key && values.contains_key(old));
        values.remove(&key);
        // Count both owners of each key, including a group's member directory.
        // An oversized key still applies to the current snapshot, but cannot
        // displace the entire session's manual choices just to persist itself.
        if key_bytes(&key).saturating_mul(2) <= MAX_KEY_BYTES {
            self.order.push_back(key.clone());
            values.insert(key, fold);
        }
        let mut bytes = values.keys().map(key_bytes).sum::<usize>()
            + self.order.iter().map(key_bytes).sum::<usize>();
        while values.len() > MAX_ENTRIES || bytes > MAX_KEY_BYTES {
            if let Some(oldest) = self.order.pop_front() {
                bytes = bytes.saturating_sub(key_bytes(&oldest));
                if let Some((key, _)) = values.remove_entry(&oldest) {
                    bytes = bytes.saturating_sub(key_bytes(&key));
                }
            } else if let Some(oldest) = values.keys().next().cloned() {
                // Also bound values restored by tests or other state owners.
                if let Some((key, _)) = values.remove_entry(&oldest) {
                    bytes = bytes.saturating_sub(key_bytes(&key));
                }
            } else {
                break;
            }
        }
        if values.capacity() > MAX_ENTRIES * 2 {
            values.shrink_to_fit();
        }
        if self.order.capacity() > MAX_ENTRIES * 2 {
            self.order.shrink_to_fit();
        }
    }
}

pub(crate) fn insert(key: FoldKey, fold: FoldState) {
    let atom = FOLD_OVERRIDES.state();
    let mut values = atom.write();
    RETENTION.lock().insert(&mut values, key, fold);
}

pub(crate) fn clear() {
    let atom = FOLD_OVERRIDES.state();
    let mut values = atom.write();
    let mut retention = RETENTION.lock();
    *values = HashMap::new();
    retention.order = VecDeque::new();
}

#[cfg(test)]
#[path = "fold_overrides_test.rs"]
mod tests;
