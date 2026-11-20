# Terminal components

`peri-tui -> peri-tui-components -> ratatui / ratatui-kit / peri-genui / peri-theme`.

This crate owns rendering and local interaction state only. It does not import
Peri Agent, Session, MCP, configuration, persistence, or application atoms. It
also owns the business-independent renderers moved down from the app: Markdown
(`markdown::*`), OpenUI fence rendering (`genui::*`), width-safe text wrapping
(`text::wrap_by_width`) and terminal text safety (`markdown::safety`).

| Primitive | Caller supplies | Component owns |
| --- | --- | --- |
| `textarea::TextArea` | text state, cursor/selection/placeholder styles, focus | rendering and wrapping |
| `textarea::TextAreaState` | editing commands | text, undo history, cursor and selection |
| `list::ListViewport` | measured content rows, row metrics, item count, selection | visible range and matching hit layout |
| `completion::CompletionPopup` | pre-styled label/detail rows, selected index, inline height, title, empty text, styles | selected marker, visible window (`ListViewport::window`) and the matching `hit_layout()`; `hit()` returns the full-list index |
| `list_nav` | key events and selection | navigation intent and bounds |
| `overlay::OverlayFrame` | title line, border/title styles, area | top/bottom borders, centred title, `inner()` region and `hit()` (inside/border/outside); never clears |
| `spinner::Spinner` | frame set, external frame index, icon style, pre-formatted text line | frame selection (`frame % len`) and `icon + space + text` composition |
| `panel_mouse` | last drawn rectangle, row metrics, mouse event | hit testing and local area tracking |
| `scrollbar::vertical_scrollbar` | thumb and arrow styles | scrollbar appearance |
| `markdown::parse_markdown*` | source text, width, palette, base foreground | typed segments (text/table/image/genui/mermaid), chunk cache |
| `markdown::table_data_to_lines` | table data, table theme, width | unicode grid layout |
| `markdown::safety` | untrusted display fields | control-character filtering and URL classification |
| `genui::render` | parsed OpenUI segment, width, theme, optional `EvalContext` | layout, styling and button hit ranges |
| `text::wrap_by_width` | text and width | grapheme/display-width wrapping without content loss |
| `mermaid` | confirmed source, closed flag, width, job results | statement confirmation, layout, overview and preview state |

The application resolves theme tokens, chooses first-frame panel height, routes
focus, translates business objects to rows and executes actions. A list index is
not a stable business identity; resolve the selected item in the application.
OpenUI interaction state stays in the app: the app passes the evaluation context
to `genui::render` and validates the session epoch separately when it dispatches
an action. No business background tasks or subscriptions are
added here. Local state is dropped with its owner. The application still owns
transcript retention.

Run `./scripts/cargo-rmcp-patched.sh run --locked -p peri-tui-components --example textarea` for deterministic buffer
output at four sizes, focused/unfocused states and empty/CJK/wrapped input.
`./scripts/cargo-rmcp-patched.sh test --locked -p peri-tui-components` exercises editing, selection, undo, navigation,
resize and hit-layout agreement. Full terminal E2E remains in `KonghaYao/peri-e2e`.

Run `./dev-tui.sh` from the repository root for the interactive storybook. Mouse
is the primary input and mirrors the real TUI:

- clicking a story or variant row switches views in one click, matching the
  panel "click as enter" rule;
- clicking inside a `TextArea` focuses it and places the caret with the app's
  rule — visual row from `wrap_text` plus display column — so wide characters
  map by cell width and clicks past the last visual row are ignored;
- clicking a list row selects that item; header/footer rows are inert and the
  scrollbar column is excluded (`panel_mouse::is_scrollbar_column`);
- the completion story paints the same inline popup used by the host's `@` and `/`
  input completion: 2-row window borders, eight content rows, and a selection that
  stays inside `ListViewport`'s window. Clicking a row selects the full-list index
  the same window painted; the empty variant has no clickable row. The wheel moves
  the selection by three rows, and the popup's own height/rows are printed below
  the frame;
- the wheel scrolls lists by three rows per notch — the host's `SCROLL_LINES` — and
  drives the scrollbar story, without moving the list selection; the bar itself has
  no click handler, as in the app;
- the Markdown and OpenUI stories feed real source through `parse_markdown` →
  typed segments → `table_data_to_lines` / `genui::render`; the wheel scrolls the
  document by the same three rows and clamps at the rendered content height.
- the overlay story centres a fixed-size popup over the transcript: the preview clears
  the popup once, `OverlayFrame` paints the borders and title, and clicks inside the
  frame confirm while outside clicks return to the host. The wheel scrolls the
  transcript only outside the frame (the HITL popup rule); the download variant shows
  the same shell without a confirm action;
- the spinner story feeds fixed frames: idle holds frame 0, while active and tool
  advance the host-owned index on click; the tool variant previews the glyph the host
  composes into a card header, so dumps stay reproducible;
- Mermaid character art never wraps: a frame keeps its natural rendered width, so
  a diagram wider than the preview gets a bottom `◄ … ►` bar. Horizontal wheel
  notches move eight columns, and Shift+wheel does the same on a vertical notch;
  the thumb can be grabbed and dragged, and clicking the track jumps to that
  position. Markdown prose still wraps at the column width, so only unwrapped
  diagrams show the bar.

Ten stories cover `textarea`, `list`, `scrollbar`, `completion` (`@` rows,
`/` label+description columns, a scrolled window and the empty state),
`markdown` (rich text, headings, lists, code, table, image, gen-ui fence,
streaming), `openui` (stack column/row, card, text, heading, list, table, key-value, progress, divider,
button, bar chart, sparkline, streaming), `overlay` (confirm and download),
`spinner` (idle, active and tool frames), `mermaid` (flowchart, sequence,
state, class, ER, pie, journey, gantt, timeline, gitGraph, mindmap, quadrant,
requirement, sankey, xychart, architecture, packet and streaming) and `charts`
— one static frame per
`tui-chart` 0.6.6 example (MIT,
Copyright (c) 2025 Sorin Albu-Irimies), themed with the storybook palette. The
`rich-text` variant is the kitchen-sink document: headings,
paragraphs with soft/hard breaks, all inline styles, inline/reference/autolinks,
escapes and entities, inline HTML stripping, nested quotes and lists, five code
block languages plus fallbacks, aligned tables, rules, images and an OpenUI fence.
The `streaming` variants hold a fixed mid-stream cursor, so dumps stay
deterministic. Playback only starts on demand: the last row of the preview is a
draggable progress bar — click or drag to seek — prefixed by an `[ auto ]` button that
plays from the current cursor to 100% in three seconds; at 100% it first rewinds to 0
and replays the whole document. Dragging the bar during playback cancels it immediately.
Markdown shows complete blocks followed by a growing code block, and the OpenUI
fence renders its finished components while the unfinished argument degrades to
a visible `gen-ui:` error until the fence closes. Mermaid loads its `.mmd`
fixtures at compile time. The static variants cover every diagram type the
component closes and renders — the seventeen `mermaid-text` kinds except
`block-beta`, whose label parsing is lossy in 0.58.0 — so unsupported types
keep their source and never appear as a story. The streaming variant
shows the diagram for the confirmed prefix, the unfinished source and any
preview notice; at 100% it validates and renders the final diagram. Large
flowcharts in the host fall back to the component's semantic overview; the
storybook keeps that path in unit tests instead of a static variant.
Dumps accept `--width`/`--height` (default `100x32`) so wide documents and narrow
layouts can be inspected without a TTY.

Navigation is mouse-only, matching the host TUI: there are no story or variant
keys. The wheel over the Stories panel scrolls the story/variant list when the
panel cannot show every entry, so the full chart gallery stays reachable at the
default `100x32` and at the `40x10` minimum. `q`, `Esc` or `Ctrl-C` quit, and
while a `TextArea` variant is focused
keys edit its text instead of leaving the storybook (only `Esc`/`Ctrl-C` still
quit there). Without a TTY, use
`./dev-tui.sh --list` for story keys and
`./dev-tui.sh --dump` (optionally `--story <key> --variant <key>`) or
`./dev-tui.sh --dump-all` for deterministic `TestBackend` snapshots. Stories and
their fixtures live in `src/bin/storybook/{main,app,stories}.rs` and the chart
gallery in `src/bin/storybook/charts/`; each story is a
`Story` of static `Variant` initial states and render functions, so adding a case
means appending a variant, not changing the widget under test. Hit testing is
covered by `src/bin/storybook/app_test.rs`.

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

The component's Markdown parser scans the original CommonMark fences with
`pulldown-cmark` 0.12; `mermaid-text = "=0.58.0"` handles diagram layout. The
application chooses widths from its viewport, dispatches due jobs to its
background worker, colors/wraps lines and owns panels, focus and i18n. The worker
executes `render_source`/`render_final` and returns the result through
`PreviewEngine::complete`; only results matching session, graph, prefix version
and layout params are published. The Mermaid pipeline declares no globals,
threads or timers.

## Boundary checks

CI and pre-commit run `python3 scripts/test-tui-components.py` and
`python3 scripts/check-tui-components.py`. These checks require Python 3.11+
for the standard-library TOML parser; select that `python3` on PATH. They cover
all direct dependency tables and source patterns, not macro expansion or a
transitive dependency audit. Approved direct dependencies are presentation
primitives plus the presentation-only workspace crates `peri-genui` and
`peri-theme`; application crates are still rejected, and approved path
dependencies must point at `../<package>`. Renderer-level `static` caches
(OnceLock/LazyLock/thread-local counters) are allowlisted per file with a
reason; `static mut` and `lazy_static!`/`atom!` are rejected everywhere. The
component depends on ratatui-kit without its optional `full` feature; workspace
feature unification is not an isolation test.

## Scope of this extraction

Spinner labels and modes remain in `peri-tui` because they encode business activity.
Panel registries, popups, transcript/tool projections and orchestration remain
application components. This is an initial extraction, not certification that
all obsolete TUI code has been removed. Full application build and visual/E2E
acceptance are required before merging; see delivery evidence for current blockers.
