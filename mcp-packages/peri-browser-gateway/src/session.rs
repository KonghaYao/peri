//! 会话准入、两个索引与关闭屏障。只有 Live 记录可以产生工作许可。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::transport::streamable_http_server::session::SessionId;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::task_tracker::TaskTrackerToken;
use tokio_util::task::TaskTracker;

use crate::config::BrowserConfig;
use crate::core::BrowserCore;
use crate::token::derive_token;

pub(crate) const SETTLE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub(crate) enum SettlementError {
    #[error("local session cleanup did not settle before its deadline")]
    Deadline,
    #[error("local transport worker could not be joined")]
    Transport,
    #[error("local resources closed, but the engine did not confirm release")]
    Unconfirmed,
}

type Settlement = Result<(), SettlementError>;

#[derive(Clone, Copy, Debug)]
pub(crate) enum CloseReason {
    Delete,
    Transport,
    Initialize,
    Shutdown,
}

enum Phase {
    Initializing {
        started: bool,
    },
    Live {
        core: Arc<BrowserCore>,
        token: String,
    },
    Closing {
        core: Option<Arc<BrowserCore>>,
    },
    Settled,
}

pub(crate) struct SessionEntry {
    phase: Mutex<Phase>,
    pub stop: CancellationToken,
    activity: TaskTracker,
    settled: watch::Sender<Option<Settlement>>,
}

impl SessionEntry {
    fn new() -> Self {
        Self {
            phase: Mutex::new(Phase::Initializing { started: false }),
            stop: CancellationToken::new(),
            activity: TaskTracker::new(),
            settled: watch::channel(None).0,
        }
    }

    pub fn start_initialize(&self) -> bool {
        let mut phase = self.phase.lock().unwrap();
        if matches!(*phase, Phase::Initializing { started: false }) {
            *phase = Phase::Initializing { started: true };
            true
        } else {
            false
        }
    }

    pub fn live(&self) -> bool {
        matches!(*self.phase.lock().unwrap(), Phase::Live { .. })
    }

    pub fn admit(&self) -> Option<(Arc<BrowserCore>, TaskTrackerToken)> {
        let phase = self.phase.lock().unwrap();
        let Phase::Live { core, .. } = &*phase else {
            return None;
        };
        Some((core.clone(), self.activity.token()))
    }

    pub fn expire_initialize(&self) {
        let mut phase = self.phase.lock().unwrap();
        if matches!(*phase, Phase::Initializing { .. }) {
            self.close(&mut phase, CloseReason::Initialize);
        }
    }

    pub fn begin_close(&self, reason: CloseReason) {
        self.close(&mut self.phase.lock().unwrap(), reason);
    }

    fn close(&self, phase: &mut Phase, reason: CloseReason) {
        let core = match &*phase {
            Phase::Initializing { .. } => None,
            Phase::Live { core, .. } => Some(core.clone()),
            Phase::Closing { .. } | Phase::Settled => return,
        };
        tracing::debug!(?reason, "关闭浏览器会话准入");
        *phase = Phase::Closing { core };
        self.activity.close();
        self.stop.cancel();
    }

    pub async fn settle_domain(&self) -> Settlement {
        let core = {
            let phase = self.phase.lock().unwrap();
            match &*phase {
                Phase::Closing { core } => core.clone(),
                _ => unreachable!("会话必须先停止准入"),
            }
        };
        self.activity.wait().await;
        if let Some(core) = core {
            core.settle().await?;
        }
        Ok(())
    }

    pub fn report_deadline(&self) {
        self.settled
            .send_replace(Some(Err(SettlementError::Deadline)));
        tracing::error!("浏览器会话清理超时；仍在等待已登记的任务");
    }

    pub async fn wait(&self) -> Settlement {
        let mut rx = self.settled.subscribe();
        let outcome = rx
            .wait_for(Option::is_some)
            .await
            .expect("sender lives with entry")
            .unwrap();
        outcome
    }
}

#[derive(Default)]
struct Entries {
    stopped: bool,
    by_id: HashMap<SessionId, Arc<SessionEntry>>,
    by_token: HashMap<String, Arc<SessionEntry>>,
    first_failure: Option<SettlementError>,
}

pub(crate) struct SessionDirectory {
    entries: Mutex<Entries>,
    pub finalizers: TaskTracker,
    pub config: Arc<BrowserConfig>,
    monitor_index: String,
    secret: Vec<u8>,
}

impl SessionDirectory {
    pub fn new(config: BrowserConfig, monitor_index: String, secret: Vec<u8>) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            finalizers: TaskTracker::new(),
            config: Arc::new(config),
            monitor_index,
            secret,
        }
    }

    pub fn create_permit(&self) -> Option<TaskTrackerToken> {
        let entries = self.entries.lock().unwrap();
        (!entries.stopped).then(|| self.finalizers.token())
    }

    pub fn initialize(&self, id: SessionId) -> Arc<SessionEntry> {
        let mut entries = self.entries.lock().unwrap();
        let entry = Arc::new(SessionEntry::new());
        entries.by_id.insert(id, entry.clone());
        if entries.stopped {
            entry.begin_close(CloseReason::Shutdown);
        }
        entry
    }

    pub fn publish(&self, entry: &Arc<SessionEntry>) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let mut phase = entry.phase.lock().unwrap();
        if entries.stopped || !matches!(*phase, Phase::Initializing { started: true }) {
            return false;
        }
        let core = BrowserCore::new(
            self.config.clone(),
            self.monitor_index.clone(),
            entry.stop.clone(),
        );
        let token = derive_token(&self.secret, core.session_id());
        entries.by_token.insert(token.clone(), entry.clone());
        *phase = Phase::Live { core, token };
        true
    }

    pub fn entry(&self, id: &SessionId) -> Option<Arc<SessionEntry>> {
        self.entries.lock().unwrap().by_id.get(id).cloned()
    }

    pub fn by_token(&self, token: &str) -> Option<Arc<SessionEntry>> {
        self.entries
            .lock()
            .unwrap()
            .by_token
            .get(token)
            .filter(|e| e.live())
            .cloned()
    }

    pub fn list(&self) -> Vec<(String, Arc<BrowserCore>)> {
        self.entries
            .lock()
            .unwrap()
            .by_id
            .values()
            .filter_map(|entry| match &*entry.phase.lock().unwrap() {
                Phase::Live { core, token } => Some((token.clone(), core.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn finish(&self, id: &SessionId, entry: &SessionEntry, outcome: Settlement) {
        let mut entries = self.entries.lock().unwrap();
        let mut phase = entry.phase.lock().unwrap();
        entries
            .by_token
            .retain(|_, other| !std::ptr::eq(entry, other.as_ref()));
        entries.by_id.remove(id);
        if let Err(error) = outcome {
            entries.first_failure.get_or_insert(error);
            tracing::error!(%error, "浏览器会话清理失败");
        }
        *phase = Phase::Settled;
        entry.settled.send_replace(Some(outcome));
    }

    pub fn begin_shutdown(&self) {
        let mut entries = self.entries.lock().unwrap();
        entries.stopped = true;
        for entry in entries.by_id.values() {
            entry.begin_close(CloseReason::Shutdown);
        }
        self.finalizers.close();
    }

    pub async fn wait_shutdown(&self) -> Settlement {
        self.finalizers.wait().await;
        self.entries
            .lock()
            .unwrap()
            .first_failure
            .map_or(Ok(()), Err)
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
