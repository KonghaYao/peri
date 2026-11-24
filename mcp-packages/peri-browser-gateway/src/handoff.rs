//! 同一会话的人工请求。此处不持有、也不创建 controller。

use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::error::BrowserError;
use crate::page::Screenshot;

pub(crate) type HumanReply = Option<Option<Screenshot>>;

pub(crate) struct Handoff {
    pub id: String,
    pub page: String,
    pub prompt: Option<String>,
    pub deadline: Instant,
    pub accepted: bool,
    pub reply: oneshot::Sender<HumanReply>,
}

pub(crate) struct OfferGuard {
    pub slot: Arc<Mutex<Option<Handoff>>>,
    pub id: String,
    pub changed: tokio::sync::watch::Sender<u64>,
}

impl Drop for OfferGuard {
    fn drop(&mut self) {
        let mut slot = self.slot.lock().unwrap();
        if slot.as_ref().is_some_and(|offer| offer.id == self.id) {
            slot.take();
            self.changed.send_modify(|v| *v += 1);
        }
    }
}

pub(crate) fn validate(
    slot: &Option<Handoff>,
    id: &str,
    page: &str,
) -> Result<Instant, BrowserError> {
    slot.as_ref()
        .filter(|offer| {
            offer.id == id
                && offer.page == page
                && Instant::now() < offer.deadline
                && !offer.accepted
        })
        .map(|offer| offer.deadline)
        .ok_or(BrowserError::StaleView)
}
