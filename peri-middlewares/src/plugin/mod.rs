pub mod config;
pub mod install_counts;
pub mod installer;
pub mod loader;
pub mod marketplace;
pub mod middleware;
pub mod types;

pub use config::{
    claude_home, claude_settings_path, installed_plugins_path, known_marketplaces_path,
    load_claude_settings, load_installed_plugins, load_known_marketplaces, load_plugin_manifest,
    marketplaces_cache_dir, plugin_cache_dir, plugins_dir, save_claude_settings_enabled_plugins,
    save_installed_plugins, save_known_marketplaces, ClaudeSettings, PluginConfigError,
};
pub use install_counts::{
    fetch_install_counts, format_install_count, is_install_counts_cache_valid, load_install_counts,
};
pub use installer::{
    check_updates, cleanup_orphaned_plugins, find_plugin_in_marketplaces, install_plugin,
    remove_from_enabled_plugins, uninstall_plugin, update_enabled_plugins, update_plugin,
    InstallerError, PluginUpdateInfo,
};
pub use loader::{
    load_enabled_plugins, load_enabled_plugins_aggregated, CommandEntry, CommandProvider,
    CommandSource, LoadedPlugin, LoaderError, PluginCommandProvider, PluginLoadResult,
};
pub use marketplace::{
    parse_marketplace_input, AvailablePlugin, MarketplaceEntry, MarketplaceError,
    MarketplaceManager, MarketplaceRefreshEvent, MarketplaceStatus,
};
pub use middleware::PluginMiddleware;
pub use types::{
    InstallScope, InstalledPlugin, InstalledPlugins, KnownMarketplace, MarketplaceManifest,
    MarketplacePlugin, MarketplaceSource, McpServerEntry, PluginAgent, PluginAuthor, PluginChannel,
    PluginCommand, PluginCommandEntry, PluginLspServer, PluginManifest, PluginOption, PluginOrigin,
};
