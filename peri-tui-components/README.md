# Terminal components

`peri-tui -> peri-tui-components -> ratatui / ratatui-kit`.

This crate owns rendering and local interaction state only. It does not import
Peri Agent, Session, MCP, configuration, persistence, or application atoms.

| Primitive | Caller supplies | Component owns |
| --- | --- | --- |
| `textarea::TextArea` | text state, cursor/selection/placeholder styles, focus | rendering and wrapping |
| `textarea::TextAreaState` | editing commands | text, undo history, cursor and selection |
| `list::ListViewport` | measured content rows, row metrics, item count, selection | visible range and matching hit layout |
| `list_nav` | key events and selection | navigation intent and bounds |
| `panel_mouse` | last drawn rectangle, row metrics, mouse event | hit testing and local area tracking |
| `scrollbar::vertical_scrollbar` | thumb and arrow styles | scrollbar appearance |

The application resolves theme tokens, chooses first-frame panel height, routes
focus, translates business objects to rows and executes actions. A list index is
not a stable business identity; resolve the selected item in the application.
No background tasks, subscriptions or process-wide caches are added here. Local
state is dropped with its owner. The application still owns transcript retention.

Run `cargo run -p peri-tui-components --example textarea` for deterministic buffer
output at four sizes, focused/unfocused states and empty/CJK/wrapped input.
`cargo test -p peri-tui-components` exercises editing, selection, undo, navigation,
resize and hit-layout agreement. Full terminal E2E remains in `KonghaYao/peri-e2e`.

## Scope of this extraction

Spinner labels and modes remain in `peri-tui` because they encode business activity.
Panel registries, popups, transcript/tool projections and orchestration remain
application components. This is an initial extraction, not certification that
all obsolete TUI code has been removed. Full application build and visual/E2E
acceptance are required before merging; see delivery evidence for current blockers.
