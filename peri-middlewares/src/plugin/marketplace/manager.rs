use std::path::PathBuf;

use chrono::Utc;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::fetch;
use super::AvailablePlugin;
use super::MarketplaceEntry;
use super::MarketplaceRefreshEvent;
use super::MarketplaceStatus;
use crate::plugin::config;
use crate::plugin::types::KnownMarketplace;
use crate::plugin::types::MarketplaceManifest;
use crate::plugin::types::MarketplacePlugin;
use crate::plugin::types::MarketplaceSource;

#[path = "manager/marketplace_manager.rs"]
mod marketplace_manager;
pub use marketplace_manager::MarketplaceManager;
