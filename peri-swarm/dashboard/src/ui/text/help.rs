//! Help-overlay wording.
//!
//! Kept in a submodule so the central text module stays inside the repository
//! size budget; every string is still reachable as `text::…` through the
//! re-export in [`super`].

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

/// Help overlay title.
pub const HELP_TITLE: &str = "Getting started";

/// Help overlay footer inside the panel.
///
/// The Esc/close hint lives in the shell footer, so this row only explains how
/// to scroll; the two never repeat each other.
pub const HELP_HINT: &str = "wheel or up/down scrolls · PgUp/PgDn pages";

/// One help section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelpSection {
    /// Section heading.
    pub title: &'static str,
    /// Body lines.
    pub lines: &'static [&'static str],
}

/// Help content.
pub const HELP_SECTIONS: [HelpSection; 5] = [    HelpSection {
        title: "Credentials",
        lines: &[
            "On first run, enter the gateway base URL and an API key.",
            "Connect runs GET /healthz and an authenticated session list before saving anything.",
            "The key is stored in ~/.config/peri-swarm-dashboard/config.toml with mode 0600 and is never displayed.",
        ],
    },
    HelpSection {
        title: "Opening a session",
        lines: &[
            "The Session log lists the most recently active sessions first.",
            "Rows are paged: use the paginator under the table or PgUp/PgDn to change page.",
            "Click a row, or focus the page and press Enter, to open that session.",
        ],
    },
    HelpSection {
        title: "Sending input and approving",
        lines: &[
            "Open a session and type in the composer at the bottom of the page.",
            "Ctrl-Enter sends the message. When a turn requests approval, approve or deny it from the session header.",
        ],
    },
    HelpSection {
        title: "Sign out",
        lines: &[
            "Settings → Sign out deletes the saved credentials and returns to onboarding.",
            "Signing out does not cancel a running turn on the gateway.",
        ],
    },
    HelpSection {
        title: "Mouse and keyboard",
        lines: &[
            "Click sidebar items, buttons, and fields. The wheel scrolls the sidebar or the page under the pointer.",
            "Tab and Shift-Tab move focus; arrows navigate; Enter activates; Esc goes back.",
            "Ctrl-C quits from anywhere. q quits only when no text field owns focus.",
        ],
    },
];
