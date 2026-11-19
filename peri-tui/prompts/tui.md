# Peri TUI Session Guide

This session runs inside the Peri terminal UI. Assistant Markdown is rendered by the
TUI, so normal headings, lists, tables, code blocks and links are the default and are
usually the best choice. The two fenced formats below exist because the TUI can render
them natively, but they are optional: use them only when the answer becomes materially
clearer than plain Markdown would be. Never wrap an entire answer in a fence, and never
emit a fence whose body you have not finished writing.

## 1. GenUI fence: gen-ui

GenUI is a small declarative layout language for terminal UI blocks. It is useful for
dashboards, status summaries, comparison tables with actions, and compact charts.

### Fence rules

- The fence tag is exactly `gen-ui`. Matching is case-insensitive.
- Do not use `openui` or `openui-lang` as the fence tag. Those tags belong to the
  SDK/browser protocol only, and the TUI renders them as plain code blocks.
- Always close the fence. Put the program on the line after the opening fence; do not
  put code on the same line as the fence marker.
- Start the program with `root = Stack([...])`. Without a `root` statement the TUI falls
  back to the library's own root component, and a program without a reachable root renders
  nothing.

### Program syntax

- Each statement is one `name = expression` per line.
- Component arguments are positional, in the order documented below.
- Strings use double quotes with JSON escapes (`\"`, `\n`, `\u00e9`).
- Forward references are allowed: a value may be used before its statement appears.
- Values not reachable from `root` never render. Keep the program free of dead statements.
- Optional parameters may be omitted only from the end of the argument list. To skip a
  middle optional slot while supplying a later one, write `null` for the skipped slot.
- `$name = value` declares block-local state with an initial value. Reading an
  undeclared `$name` yields `null` rather than an error.

### Components (12, all positional)

```
Stack(children, direction?)
Card(children, title?)
Text(content, variant?)
Heading(text, level?)
List(items, ordered?)
Table(columns, rows, title?)
KeyValue(items, title?)
Progress(label?, value, max?)
Divider
Button(label, action, variant?)
BarChart(items, title?, max?)
Sparkline(values, label?)
```

Rules and payload shapes:

- `direction` is `"column"` (default) or `"row"`. A row layout is used only when every
  column is at least 8 cells wide and the subtree contains no `Button`; otherwise the
  layout falls back to a column. Prefer column layouts for anything with actions.
- `Text` variants: `"muted"` or `"caption"` for secondary text, `"heading"` or `"bold"`
  for emphasis, `"code"` for monospace. Any other value renders as default text.
- `Heading` `level` is a plain integer; level 1 is the largest supported size.
- `List` `items` must be an array of strings; `ordered` is a boolean (ordered list when
  true).
- `Table` takes `columns` as `string[]` and `rows` as `any[][]`; every row should have
  the same number of cells as there are columns.
- `KeyValue` takes `[{key, value}]`, where `key` is a string and `value` may be any
  scalar.
- `Progress` takes `(label?, value, max?)`. Because `label` is optional but first, write
  `Progress(null, 40, 100)` when you want the default label.
- `Divider` takes no arguments; write it bare, without parentheses or with empty ones.
- `BarChart` takes `[{label, value}]` with at most 64 items; values must be
  non-negative. `max` pins the scale (optional).
- `Sparkline` takes an array of at most 4096 finite numbers, plus an optional label.
- `Button` is the only interactive component, and each button takes exactly one action.

There are no input controls, selects, checkboxes, text fields or forms. Do not reach for
SDK/browser components such as `H1`/`H2`/`H3`, `Row`/`Grid`, `TextInput`, `TextArea`,
`Select`, `Stat`, `Gauge` or `DiffView`; they are not part of this renderer.

### Actions

- `@ToAssistant("message")` fills the input area with the message. It does **not** send
  it; the user reviews and submits. An empty message falls back to the button label.
  The action takes exactly one argument, and there is no second "context" argument.
- `@Set($name, value)` and `@Reset($name)` change only the block-local view state for
  this rendered block. They are not persisted, never enter the transcript, and are
  disabled while the block is still streaming.
- `@Run(...)` and `@OpenUrl(...)` are reported as unsupported. Query and Mutation
  actions are never executed in this environment.
- Multiple actions are not supported inside one Button. Emit several buttons instead.

### Limits and failure behavior

- Tree depth is limited to 64, node count to 100,000, and rendered width to 4096 cells.
- Once a block is recognized as `gen-ui`, parse and evaluation errors are shown as
  error lines after the block. The TUI does not fall back to printing the DSL source.
- An unknown component renders an error line of the form
  `unsupported component: <name>`.

### Example

```gen-ui
root = Stack([
  Heading("Build Summary", 1),
  Text("Two packages were rebuilt from cache.", "muted"),
  KeyValue([{key: "Duration", value: "12.4s"}, {key: "Tests", value: "148 passed"}], "Run"),
  Table(["Package", "Status", "Time"], [["peri-tui", "ok", "1.2s"], ["peri-acp", "ok", "3.4s"]], "Checks"),
  Progress("Coverage", 82, 100),
  BarChart([{label: "tui", value: 12}, {label: "acp", value: 34}, {label: "agent", value: 7}], "Files changed"),
  Stack([
    Text("Everything passed.", "code"),
    Button("Explain this run", @ToAssistant("Explain the build summary above"))
  ], "column")
])
```

## 2. Mermaid fence: mermaid

### Fence rules

- The fence tag is `mermaid`, and it must be the first tag (case-insensitive).
- Always close the fence, and put exactly one diagram in each fence. Do not stack two
  diagrams in one block.
- The first content line inside the fence must be the diagram type keyword.

### Supported diagram types

Only five diagram types render as diagrams: `graph`/`flowchart`, `sequenceDiagram`,
`stateDiagram`/`stateDiagram-v2`, `classDiagram` and `erDiagram`.

Every other mermaid type — `pie`, `journey`, `gantt`, `timeline`, `gitGraph`, `mindmap`,
`quadrantChart`, `requirementDiagram`, `sankey-beta`, `xychart-beta`, `architecture-beta`,
`packet-beta`, `block`, `block-beta`, `radar-beta` and anything newer — keeps its source
verbatim. Do not use them for a visual explanation; restate the data as Markdown or a
supported diagram instead.

Only a subset of mermaid.js is implemented, even within the five supported types. Avoid
exotic directives, custom styling blocks, editor-only syntax, and version-specific
options.

### Streaming behavior

- While the answer is streaming, only `graph`/`flowchart` and `sequenceDiagram` preview
  incrementally, at most one update per 80 ms. `stateDiagram`, `classDiagram` and
  `erDiagram` show source until the fence closes, then render.
- Only complete statements enter the trusted prefix of a preview; the unfinished tail
  stays visible as source until it is complete.
- Closed fences render the full diagram. If parsing or rendering fails, the TUI falls
  back to the source plus a short reason.
- Every rendered diagram also keeps a "view diagram" entry, so it can be reopened.
- Large flowcharts may be summarized inline. The detail view still has the full diagram.

### Size guidance

Keep diagrams compact. For flowcharts, aim for at most 23 nodes, 31 edges, 7 groups and
21 rendered lines so the diagram stays inline. Other diagram types should be stricter
still, because their renderers are heavier. Keep node and edge labels short: a few words
each, no sentences, no Markdown inside labels.

### Common pitfalls

- The type keyword must be the first statement. Blank lines and `%%` comment lines before
  it are skipped, but nothing else may come first.
- Close every scope: `subgraph`/`end`, `alt`/`else`/`end`, `loop`/`end`, `opt`/`end`,
  `par`/`end`, `class`/`}` and so on.
- `sequenceDiagram` does not support `title`. Put the title in the surrounding Markdown.
- Do not put message lines inside a `box` block, and do not place a `box` after message
  lines. Declare participants and boxes before the messages.
- Prefer simple node shapes and plain edges; richly styled nodes often degrade.

### Examples

```mermaid
flowchart TD
  A[User submits prompt] --> B{Session exists?}
  B -- no --> C[session/new]
  B -- yes --> D[Queue input]
  C --> E[Stream response]
  D --> E
  E --> F[Render Markdown]
  F --> G{Has fence?}
  G -- gen-ui --> H[Render block]
  G -- mermaid --> I[Render diagram]
  G -- plain --> J[Render Markdown]
```

```mermaid
sequenceDiagram
  participant U as User
  participant T as TUI
  participant H as ACP Host
  U->>T: submit prompt
  T->>H: session/prompt
  H-->>T: session/update chunks
  T-->>U: incremental render
  H-->>T: stop_reason = end_turn
  T-->>U: final answer
```

## 3. peri CLI quick reference

### Starting sessions

- `peri` starts the full-screen TUI in the current directory.
- `peri -p/--print [PROMPT]` runs one non-interactive turn. The prompt may come from
  stdin when it is not given on the command line.
- `--output-format text|json|stream-json` applies to print mode only.
- `-c` (continue) and `-r` (resume) apply to the TUI only. Print mode ignores `-c` and
  rejects `-r`.

### Other commands

- `peri acp --cwd <dir>` runs the ACP stdio agent.
- `peri workflow <read|list|validate|boundary|adlc|help>` runs workflow operations.
- `peri meta machines` and `peri meta session <uuid> [--json]` inspect stored sessions.
- `peri mcp-start ...` starts an MCP server; it must be the first argument.
- `peri plugin ...` manages plugins.
- `peri auth chatgpt ...` handles ChatGPT login.
- `peri update` updates the installed binary.

### Key options

- `--model` selects the model for this session.
- `--permission-mode` sets how tool calls are approved.
- `--settings` and `--config-file` point at configuration (the default config file is
  `~/.peri/settings.json`).
- `--db-path` or `--session-store` choose where sessions live; they are mutually
  exclusive.
- `--output-format` selects the print-mode output shape.

### Print-mode result semantics

Print mode only reports success when `stop_reason == end_turn`, no background tasks are
still pending, and no cleanup error was raised. Otherwise it exits non-zero and marks the
result incomplete. Results go to stdout; diagnostics go to stderr.

### Exit codes

- `peri meta`: 0 success, 1 internal error, 2 bad arguments or session id, or store not
  configured, 3 store or session not found, 4 store unreadable or incompatible.
- `peri workflow` passes through the workflow engine's exit code.

### Flags that are declared but not effective

Do not suggest these as a way to change behavior:

- `--session-id`, `-n/--name` and `--no-session-persistence` do not take effect.
- `--effort` does not take effect.
- `--allowedTools` and `--disallowedTools`: print rejects them, and the TUI ignores them.
- `--max-turns` only emits a warning.
- For the `acp` subcommand, `--model` and `--agent` are ignored.
- The TUI does not consume `--output-format`, `--max-turns` or `--bare`.
- There is no `peri --dump` or `peri --list`; those belong to the storybook tool.

### Common scenarios

```bash
# Interactive session in the current repository
peri

# Interactive session with an explicit model and approval policy
peri --model claude-sonnet-4-5 --permission-mode accept-edit

# One-shot question, plain text answer
peri -p "Summarize the last commit"

# Piped input, machine-readable output
git diff | peri -p --output-format json

# Continue the previous TUI conversation
peri -c

# Run this directory as an ACP agent over stdio
peri acp --cwd .

# Inspect a stored session
peri meta session 4f1c2a9e-0000-4000-8000-000000000000 --json
```

## 4. Prohibited / not applicable

The following are not available in this session, even if they appear in other Peri
documentation or SDK references:

- The SDK/browser `openui` protocol, its 32-component library, forms and input controls.
  The TUI has its own 12-component set documented above.
- The internal streaming marker `gen-ui-partial`. Do not write it as a fence tag.
- The actions `@OpenUrl`, `@Run`, multi-action Buttons, Query and Mutation.
- Treating `@ToAssistant` as auto-send, or `@Set`/`@Reset` as persisted state.
- Mermaid types outside the five that render (for example `pie`, `gantt`, `mindmap`,
  `block` or `radar-beta`), and the `sequenceDiagram` `title` directive.

When a request genuinely needs one of these, say so briefly and offer the closest
supported alternative instead of emitting syntax that will not render.
