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
| `mermaid` | confirmed source, closed flag, width, job results | statement confirmation, layout, overview and preview state |

The application resolves theme tokens, chooses first-frame panel height, routes
focus, translates business objects to rows and executes actions. A list index is
not a stable business identity; resolve the selected item in the application.
No background tasks, subscriptions or process-wide caches are added here. Local
state is dropped with its owner. The application still owns transcript retention.

Run `./scripts/cargo-rmcp-patched.sh run --locked -p peri-tui-components --example textarea` for deterministic buffer
output at four sizes, focused/unfocused states and empty/CJK/wrapped input.
`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui-components` exercises editing, selection, undo, navigation,
resize and hit-layout agreement. Full terminal E2E remains in `KonghaYao/peri-e2e`.

## Text input composition

`TextArea` renders into a buffer. Its `base_style` and `width_reserve` builders
allow an application to customize the existing fallback/padding style and wrapping
width without changing the origin. Defaults remain black background and two
reserved columns; `show_cursor` is explicitly supplied from application focus.
When the cursor is visible, at least one column is reserved for its trailing cell;
`width_reserve(0)` uses the full width only when the cursor is hidden. A viewport
only one cell wide can still clip a character or its trailing cursor.
The component has no loading state: application status controls borders/labels.

For composition with surrounding lines, call the same renderer with named props:

```rust
use peri_tui_components::textarea::{
    render_multiline_with_cursor, TextAreaRenderOptions, TextAreaStyles,
};
use ratatui::style::Style;

let lines = render_multiline_with_cursor(TextAreaRenderOptions {
    text: "hello 世界",
    cursor: 5,
    selection_range: None,
    placeholder: None,
    styles: TextAreaStyles {
        cursor: Style::default(),
        selection: Style::default(),
        placeholder: Style::default(),
        base: Style::default(),
    },
    max_width: 40,
    viewport_height: 3,
    show_cursor: true,
});
```

`styles.base` retains the existing cursor-line fallback and row-padding semantics;
non-cursor spans may inherit their containing line's style. It is not a blanket
style applied to the whole buffer. Both rendering APIs share one implementation.

## Mermaid pipeline

`peri_tui_components::mermaid` owns the pure Mermaid pipeline:

| Module | Responsibility |
| --- | --- |
| `stream` | confirms complete statements from streamed source |
| `frame` | lays out a full or trusted prefix with `mermaid-text` |
| `overview` | derives groups, cross-group edges and path summaries for large flowcharts |
| `layout` | shares geometry by source, exact width, engine version and layout params |
| `preview` | trusted-prefix state machine, 80 ms commit coalescing, cancellation and version checks |
| `fence` | fence metadata and stable graph identity (`session + message + open_start`) |

The application still scans CommonMark fences, chooses widths from its viewport,
dispatches due jobs to its background worker, colors/wraps lines and owns panels,
focus and i18n. The worker executes `render_source`/`render_final` and returns the
result through `PreviewEngine::complete`; only results matching session, graph,
prefix version and layout params are published. The component declares no globals,
threads or timers; `mermaid-text = "=0.58.0"` is the only added dependency.

## Boundary checks

CI and pre-commit run `python3 scripts/test-tui-components.py` and
`python3 scripts/check-tui-components.py`. These checks require Python 3.11+
for the standard-library TOML parser; select that `python3` on PATH. They cover
all direct dependency tables and source patterns, not macro expansion or a
transitive dependency audit. The component depends on ratatui-kit without its
optional `full` feature; workspace feature unification is not an isolation test.

## Scope of this extraction

Spinner labels and modes remain in `peri-tui` because they encode business activity.
Panel registries, popups, transcript/tool projections and orchestration remain
application components. This is an initial extraction, not certification that
all obsolete TUI code has been removed. Full application build and visual/E2E
acceptance are required before merging; see delivery evidence for current blockers.
