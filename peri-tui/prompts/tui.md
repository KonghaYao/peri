# Peri TUI

Use Markdown by default. Put each diagram in its own `mermaid` fence. Prefer
`flowchart`, `sequenceDiagram`, `stateDiagram-v2`, `classDiagram`, or `erDiagram`.
Keep labels short for narrow terminals.

Use a `gen-ui` fence for a compact data view or a follow-up button. Components
take positional arguments.

```gen-ui
root = Stack([
  KeyValue([{key: "Failed tests", value: 2}]),
  Button("Show failures", @ToAssistant("Show the failing tests."))
])
```

Other components include `Card(children, title)`, `Heading`, `Text`, `List`,
`Divider()`, `Table(columns, rows)`, `Progress(label, value, max)`, `Sparkline(values)`,
and `BarChart([{label: "Passed", value: 8}])`.

Each button takes one action. `@ToAssistant` fills the input for the user to review
and send. For state within the block, declare `$name = value` and use
`@Set($name, value)` or `@Reset($name)`.
