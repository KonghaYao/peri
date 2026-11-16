use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::plugin::types::{MarketplaceManifest, MarketplaceSource, PluginAuthor};

mod fetch;
mod manager;

pub use manager::MarketplaceManager;

#[cfg(test)]
#[path = "marketplace_test.rs"]
mod tests;

mod marketplace_entry;
pub use marketplace_entry::find_marketplace_json;
pub use marketplace_entry::parse_marketplace_input;
pub use marketplace_entry::read_manifest_from_path;
pub use marketplace_entry::refresh_marketplace;
pub use marketplace_entry::AvailablePlugin;
pub use marketplace_entry::MarketplaceEntry;
pub use marketplace_entry::MarketplaceError;
pub use marketplace_entry::MarketplaceRefreshEvent;
pub use marketplace_entry::MarketplaceStatus;
