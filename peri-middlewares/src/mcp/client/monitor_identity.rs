//! Host-side identity of the builtin `monitor` instance.
//!
//! Both the terminal-reminder delivery in `subscription_task_binding` and the
//! alert delivery path in the task subscription need the same identity check.
//! It lives here so neither path depends on the other.

use super::McpClientPool;

/// Reserved builtin instance name for the monitor server.
pub(crate) const MONITOR_INSTANCE: &str = "monitor";

impl McpClientPool {
    /// Whether a connected server is the builtin monitor instance.
    ///
    /// The authoritative identity is the runtime-owned
    /// `ConfigSource::Builtin { instance }`, which user configuration cannot
    /// forge. The reserved name is only a fallback for directly constructed
    /// pools (tests / local wiring): a non-builtin connection named `monitor`
    /// would also match it and inherit the monitor reminder semantics. Normal
    /// configuration cannot reach that state — `monitor` is a reserved builtin
    /// instance name, and `apply_builtin_overlay` rejects any `command`/`url`
    /// takeover at load time, even when `PERI_MCP_BUILTIN=off`.
    ///
    /// This identity selects the monitor reminder semantics: parse the
    /// `_meta["peri/monitor"]` projection for running alerts, merge unreported
    /// hits into the terminal reminder, and drop that reminder when the
    /// initiator is offline instead of failing settlement. It is never a
    /// settlement gate. A name-matched server whose `tasks/get` omits that
    /// metadata just loses the alert counts; its terminal tasks still settle.
    pub(crate) fn is_monitor_server(&self, server: &str) -> bool {
        if server == MONITOR_INSTANCE {
            return true;
        }
        self.clients.read().get(server).is_some_and(|client| {
            matches!(
                client.source.as_ref(),
                Some(crate::mcp::config::ConfigSource::Builtin { instance })
                    if instance == MONITOR_INSTANCE
            )
        })
    }

    /// Whether an owner currently has a live inbox.
    pub(crate) fn owner_online(&self, owner: &str) -> bool {
        self.session_bindings.read().inbox(owner).is_some()
    }
}
