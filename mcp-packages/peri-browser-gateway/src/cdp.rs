//! CDP page connections, re-exported from the shared `peri-browser-cdp` crate.
//!
//! The implementation lives in `peri-browser-cdp` so the MCP server and the
//! monitor share one page-connection layer, input mapping and screencast frame
//! handling (README §11). Gateway call sites keep using `crate::cdp::*`.
//!
//! Only the gateway's own owner name stays here: a page controller identifies
//! itself per process, so that policy belongs to the consumer, not the shared
//! transport.

pub use peri_browser_cdp::*;

/// Owner name the gateway uses for its writable page connection.
///
/// It is deliberately distinct from the MCP's owner: the two never hold the same
/// page at once, and a distinct name makes a mistaken double attach a visible
/// Kitesurf error instead of a silent second writer.
pub const CONTROLLER_OWNER: &str = "peri-browser-gateway";
