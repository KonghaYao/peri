//! Bounded, process-local pre-image store for session file rewinds.
//!
//! `Write` replaces a whole file, so Git `HEAD` cannot answer what the user had
//! before the call (untracked files have no HEAD entry, dirty tracked files differ
//! from it). The Workspace server records the exact bytes around every successful
//! `Write`/`Edit` and `/rewind` restores those bytes. Records are process-local:
//! a Workspace that lost them (restart, reconnect) must refuse the reversal
//! rather than guess a replacement.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
};

use parking_lot::Mutex;

/// Per-record cap. Larger writes still succeed but cannot be rewound.
const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
/// Total cap; oldest records are evicted first.
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub(crate) path: PathBuf,
    /// Bytes before the tool call; `None` means the file did not exist.
    pub(crate) pre: Option<Vec<u8>>,
    /// Bytes after the successful tool call.
    pub(crate) post: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    owner: Option<String>,
    tool_call_id: String,
}

#[derive(Default)]
struct Inner {
    records: HashMap<Key, Snapshot>,
    order: VecDeque<Key>,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct RewindSnapshots {
    inner: Mutex<Inner>,
}

impl RewindSnapshots {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Store one call's pre/post image. Oversized or over-budget records are
    /// dropped; a later rewind then refuses instead of restoring a partial image.
    pub(crate) fn record(&self, owner: Option<&str>, tool_call_id: &str, snapshot: Snapshot) {
        let size = snapshot.pre.as_deref().map_or(0, <[u8]>::len) + snapshot.post.len();
        if size > MAX_RECORD_BYTES {
            return;
        }
        let key = Key {
            owner: owner.map(str::to_owned),
            tool_call_id: tool_call_id.to_owned(),
        };
        let mut inner = self.inner.lock();
        if let Some(previous) = inner.records.remove(&key) {
            inner.bytes -= record_bytes(&previous);
            inner.order.retain(|candidate| candidate != &key);
        }
        inner.bytes += size;
        inner.order.push_back(key.clone());
        inner.records.insert(key, snapshot);
        while inner.bytes > MAX_TOTAL_BYTES {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some(removed) = inner.records.remove(&oldest) {
                inner.bytes -= record_bytes(&removed);
            }
        }
    }

    pub(crate) fn get(&self, owner: Option<&str>, tool_call_id: &str) -> Option<Snapshot> {
        let key = key(owner, tool_call_id);
        self.inner.lock().records.get(&key).cloned()
    }
}

fn key(owner: Option<&str>, tool_call_id: &str) -> Key {
    Key {
        owner: owner.map(str::to_owned),
        tool_call_id: tool_call_id.to_owned(),
    }
}

fn record_bytes(snapshot: &Snapshot) -> usize {
    snapshot.pre.as_deref().map_or(0, <[u8]>::len) + snapshot.post.len()
}
