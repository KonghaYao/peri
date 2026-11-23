//! 一个会话自有页面的 meta、诊断与快照。写入许可只从 PageControl 获得。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::cdp::{CdpEvent, NavigationState};
use crate::config::BrowserConfig;
use crate::diagnostics::Diagnostics;
use crate::snapshot::SnapshotBook;

mod control;
mod tools;

pub(crate) use control::{PageOperation, Role};

#[derive(Clone, Default)]
pub(crate) struct PageMeta {
    pub url: String,
    pub title: String,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

pub(crate) struct PageHandle {
    pub id: String,
    pub target_id: String,
    pub endpoint: String,
    pub control: control::PageControl,
    owner: String,
    connect_timeout: Duration,
    page_wait: Duration,
    meta: Mutex<PageMeta>,
    diagnostics: Mutex<Diagnostics>,
    snapshot: Mutex<SnapshotBook>,
    expired: AtomicBool,
    changed: watch::Sender<u64>,
}

impl PageHandle {
    pub fn new(
        target_id: String,
        endpoint: String,
        config: &BrowserConfig,
        changed: watch::Sender<u64>,
        stop: CancellationToken,
    ) -> Arc<Self> {
        let id = format!("p_{}", uuid::Uuid::new_v4().simple());
        Arc::new(Self {
            owner: id.clone(),
            id,
            target_id,
            endpoint,
            control: control::PageControl::new(changed.clone(), stop.child_token()),
            connect_timeout: config.connect_timeout,
            page_wait: config.page_wait,
            meta: Mutex::new(PageMeta {
                url: "about:blank".to_owned(),
                ..PageMeta::default()
            }),
            diagnostics: Mutex::new(Diagnostics::new()),
            snapshot: Mutex::new(SnapshotBook::default()),
            expired: AtomicBool::new(false),
            changed,
        })
    }

    pub fn expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }

    pub async fn fence(&self) {
        self.expired.store(true, Ordering::Release);
        self.control.fence();
        self.invalidate_snapshot().await;
        self.changed.send_modify(|v| *v += 1);
    }

    pub async fn meta(&self) -> PageMeta {
        self.meta.lock().await.clone()
    }

    pub async fn set_navigation(&self, state: &NavigationState) {
        *self.meta.lock().await = PageMeta {
            url: state.url.clone(),
            title: state.title.clone(),
            can_go_back: state.can_go_back,
            can_go_forward: state.can_go_forward,
        };
        self.changed.send_modify(|v| *v += 1);
    }

    pub async fn invalidate_snapshot(&self) {
        self.snapshot.lock().await.invalidate();
    }

    async fn reset_diagnostic_domains(&self) {
        self.diagnostics.lock().await.reset_domains();
    }

    async fn handle_event(&self, event: CdpEvent) {
        match event.method.as_str() {
            "Peri.targetExpired" => self.fence().await,
            "DOM.documentUpdated"
            | "Runtime.executionContextsCleared"
            | "Runtime.executionContextDestroyed" => self.invalidate_snapshot().await,
            "Page.frameNavigated" if event.params["frame"].get("parentId").is_none() => {
                self.invalidate_snapshot().await;
                if let Some(url) = event.params["frame"]["url"].as_str() {
                    let mut meta = self.meta.lock().await;
                    if meta.url != url {
                        meta.title.clear();
                    }
                    meta.url = url.to_owned();
                    self.changed.send_modify(|v| *v += 1);
                }
            }
            _ => {}
        }
        self.diagnostics
            .lock()
            .await
            .observe(&event.method, &event.params);
    }
}

/// JPEG bytes ready to become MCP image content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Screenshot {
    pub data: String,
    pub mime_type: String,
}

#[cfg(test)]
#[path = "page_test.rs"]
mod tests;
