//! Footer hints.
//!
//! The hint must describe what the current focus actually does: a focused text
//! field swallows the single-letter shortcuts, and a modal has its own keys.
//! Every string is reachable as `text::…` through the re-export in [`super`].

// ---------------------------------------------------------------------------
// Main pages
// ---------------------------------------------------------------------------

/// Footer hints on wide main pages whose focus is not a text field.
pub const FOOTER_MAIN: &str =
    "Tab focus · r refresh · n new session · h help · q quit · Ctrl-C quit";

/// Footer hints on narrow main pages where the sidebar is behind the menu button.
pub const FOOTER_MAIN_NARROW: &str =
    "menu/Tab focus · r refresh · n new session · h help · q quit · Ctrl-C quit";

/// Footer hints while the sidebar owns focus.
///
/// Up/Down change the page here; PgUp/PgDn belong to the content pane, so they
/// are not advertised until focus moves right.
pub const FOOTER_SIDEBAR: &str =
    "up/down change page · Right or Tab to content · h help · q quit · Ctrl-C quit";

// ---------------------------------------------------------------------------
// Focused fields
// ---------------------------------------------------------------------------

/// Footer hints while a form text field owns focus.
pub const FOOTER_FIELD: &str = "Tab next control · Enter submit · Esc leave field · Ctrl-C quit";

/// Footer hints while the SQL editor owns focus.
pub const FOOTER_QUERY_FIELD: &str =
    "Ctrl-Enter run · Esc leave field · Tab next control · Ctrl-C quit";

/// Footer hints while the session-log search owns focus.
pub const FOOTER_SEARCH_FIELD: &str =
    "type to filter · Enter/Esc leave field · Tab next control · Ctrl-C quit";

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

/// Footer hints for the Getting-started overlay.
pub const FOOTER_HELP: &str = "Esc close · click outside to dismiss";

/// Footer hints for the narrow navigation menu.
pub const FOOTER_MENU: &str = "up/down select · Enter open · Esc close";

/// Footer hints for confirmation dialogs.
///
/// The safe choice is focused first, so Enter alone cancels; the hint says what
/// the keys really do instead of promising that Enter confirms.
pub const FOOTER_CONFIRM: &str =
    "Tab or left/right select · Enter activates the focused button · Esc cancels";

/// Footer hints for the connection-problem actions.
pub const FOOTER_ERROR_ACTIONS: &str = "left/right select · Enter activate · Esc close";

// ---------------------------------------------------------------------------
// Full-screen forms
// ---------------------------------------------------------------------------

/// Footer hints on Settings.
pub const FOOTER_SETTINGS: &str =
    "Tab focus · Enter activate · Esc back · h help · q quit · Ctrl-C quit";

/// Footer hints on the too-small screen.
pub const FOOTER_TOO_SMALL: &str = "Ctrl-C quit";
