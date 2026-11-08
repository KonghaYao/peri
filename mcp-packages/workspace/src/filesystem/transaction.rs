//! Write/Edit 共用的进程内 per-target 事务。

use std::{
    collections::HashMap,
    future::Future,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use parking_lot::Mutex as TargetMutex;
use peri_acp_types::sentinel::projection_sentinel_counts_v1;

pub(crate) const SENTINEL_REJECTION: &str =
    "File change rejected because the result contains newly introduced protected projection text.";

type Registry = Mutex<HashMap<PathBuf, Weak<TargetMutex<()>>>>;

/// Pre/post bytes of one successful locked Write/Edit commit.
///
/// `pre` is `None` when the target did not exist. `post` is exactly the payload
/// this transaction wrote, not a later read of the shared path.
#[derive(Debug)]
pub(crate) struct CommittedImage {
    pub(crate) path: PathBuf,
    pub(crate) pre: Option<Vec<u8>>,
    pub(crate) post: Vec<u8>,
}

tokio::task_local! {
    static COMMIT_CAPTURE: Arc<Mutex<Option<CommittedImage>>>;
}

/// Run `future` while capturing the first successful Write/Edit commit.
///
/// The image is produced inside the per-target lock, so a cooperating writer
/// cannot interleave between its pre and post halves. Rewind consumes this
/// image instead of reading the shared path again after the tool returns.
pub(crate) async fn capture_committed_image<F: Future>(
    future: F,
) -> (F::Output, Option<CommittedImage>) {
    let slot = Arc::new(Mutex::new(None));
    let output = COMMIT_CAPTURE.scope(Arc::clone(&slot), future).await;
    let captured = slot.lock().ok().and_then(|mut slot| slot.take());
    (output, captured)
}

pub(crate) struct LockedTarget<'a> {
    path: &'a Path,
}

pub(crate) fn with_target_lock<T>(path: &Path, operation: impl FnOnce(LockedTarget<'_>) -> T) -> T {
    let lock = target_lock(path);
    let _guard = lock.lock();
    operation(LockedTarget { path })
}

impl LockedTarget<'_> {
    pub(crate) fn read_pre(&self) -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Commit `post`, then publish this transaction's pre/post image to the
    /// armed capture scope. Both halves come from the same lock acquisition.
    pub(crate) fn guard_and_commit(
        &self,
        pre: Option<Vec<u8>>,
        post: Vec<u8>,
    ) -> Result<(), CommitError> {
        if introduces_projection_sentinel_bytes(pre.as_deref().unwrap_or_default(), &post) {
            return Err(CommitError::Sentinel);
        }
        self.replace(&post)?;
        let image = CommittedImage {
            path: self.path.to_path_buf(),
            pre,
            post,
        };
        // Capture only affects rewind bookkeeping; a write outside a capture
        // scope is still a successful commit.
        let _ = COMMIT_CAPTURE.try_with(|slot| {
            if let Ok(mut slot) = slot.lock() {
                if slot.is_none() {
                    *slot = Some(image);
                }
            }
        });
        #[cfg(test)]
        wait_commit_gate(self.path);
        Ok(())
    }

    /// Atomically replace the target without the model-write sentinel guard.
    ///
    /// Rewind restores bytes the file already held before a tool call, so a
    /// restored projection sentinel line is not a new model-write introduction.
    pub(crate) fn replace(&self, post: &[u8]) -> Result<(), CommitError> {
        let parent = self.path.parent().ok_or_else(|| {
            CommitError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("target has no parent: {}", self.path.display()),
            ))
        })?;
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(CommitError::Io)?;
        }

        let tmp_path = self
            .path
            .with_extension(format!("tmp.{}", uuid::Uuid::now_v7()));
        if let Err(error) = std::fs::write(&tmp_path, post) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(CommitError::Io(error));
        }

        if let Ok(metadata) = std::fs::metadata(self.path) {
            #[cfg(unix)]
            if let Err(error) = std::fs::set_permissions(&tmp_path, metadata.permissions()) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(CommitError::Io(error));
            }
            #[cfg(not(unix))]
            let _ = metadata;
        }

        if let Err(error) = std::fs::rename(&tmp_path, self.path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(CommitError::Io(error));
        }
        Ok(())
    }
}

fn introduces_projection_sentinel_bytes(pre: &[u8], post: &[u8]) -> bool {
    let counts = |bytes: &[u8]| {
        let mut counts = HashMap::new();
        for line in bytes.split(|byte| *byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            for (sentinel, count) in projection_sentinel_counts_v1(line) {
                *counts.entry(sentinel).or_default() += count;
            }
        }
        counts
    };
    let pre_counts = counts(pre);
    counts(post)
        .into_iter()
        .any(|(sentinel, count)| count > pre_counts.get(&sentinel).copied().unwrap_or(0))
}

#[cfg(test)]
static COMMIT_GATES: OnceLock<Mutex<HashMap<PathBuf, Arc<CommitGate>>>> = OnceLock::new();

/// Test-only one-shot gate fired after a locked commit, before the lock is released.
///
/// Lets a regression simulate an uncoordinated writer landing between the
/// tool's commit and any snapshot bookkeeping.
#[cfg(test)]
pub(crate) struct CommitGate {
    pub(crate) entered: tokio::sync::Notify,
    pub(crate) release: std::sync::Barrier,
}

#[cfg(test)]
impl CommitGate {
    pub(crate) fn new() -> Self {
        Self {
            entered: tokio::sync::Notify::new(),
            release: std::sync::Barrier::new(2),
        }
    }
}

#[cfg(test)]
pub(crate) fn gate_next_commit(path: &Path, gate: Arc<CommitGate>) {
    COMMIT_GATES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(logical_path_key(path), gate);
}

#[cfg(test)]
fn wait_commit_gate(path: &Path) {
    let gate = COMMIT_GATES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&logical_path_key(path));
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.release.wait();
    }
}

#[derive(Debug)]
pub(crate) enum CommitError {
    Sentinel,
    Io(std::io::Error),
}

/// Failure from a conditional rewind replacement.
#[derive(Debug)]
pub(crate) enum RewindCommitError {
    /// The file no longer matches the state the rewind planned against.
    ChangedSinceValidation,
    Commit(CommitError),
}

/// Apply one rewound file under the same per-target lock as Write/Edit.
///
/// The caller's plan may be stale by the time it is applied. Re-reading under the
/// shared lock keeps in-process tool calls and this replacement mutually exclusive;
/// `replace` writes through a temp file so a failed write never truncates the
/// target. External writers are not coordinated by this lock: a write that lands
/// after the re-read can still be overwritten, because regular files provide no
/// portable compare-and-swap primitive.
pub(crate) fn replace_if_unchanged(
    path: &Path,
    expected: Option<&[u8]>,
    replacement: Option<&[u8]>,
) -> Result<(), RewindCommitError> {
    with_target_lock(path, |locked| {
        let actual = locked
            .read_pre()
            .map_err(|error| RewindCommitError::Commit(CommitError::Io(error)))?;
        if actual.as_deref() != expected {
            return Err(RewindCommitError::ChangedSinceValidation);
        }
        match replacement {
            Some(bytes) => locked.replace(bytes).map_err(RewindCommitError::Commit),
            None => std::fs::remove_file(path)
                .map_err(|error| RewindCommitError::Commit(CommitError::Io(error))),
        }
    })
}

pub(crate) fn logical_path_key(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn target_lock(path: &Path) -> Arc<TargetMutex<()>> {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    let registry = REGISTRY.get_or_init(Default::default);
    let key = logical_path_key(path);
    let mut entries = registry.lock().unwrap();
    entries.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = entries.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(TargetMutex::new(()));
    entries.insert(key, Arc::downgrade(&lock));
    lock
}
