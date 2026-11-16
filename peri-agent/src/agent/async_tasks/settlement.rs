use crate::agent::async_tasks::registry::BackgroundRegistryError;
use crate::agent::async_tasks::registry::BackgroundTaskRegistry;
use crate::agent::events::BackgroundTaskResult;
use peri_acp_types::tasks::{BgRegistryEvent, BgTaskKind, OnBgCompleteFn, TaskRegistrationTicket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[path = "delivery_retry.rs"]
mod delivery_retry;

pub(super) use registry_settlement::PendingTaskDelivery;

#[cfg(test)]
#[path = "settlement_test.rs"]
mod tests;

#[path = "settlement/registry_settlement.rs"]
mod registry_settlement;
