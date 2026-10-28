// Vendored from peri-canvas@0.1.0 (packages/canvas/src/index.ts), trimmed to the
// components peri-cf actually renders. License declared MIT in
// packages/canvas/package.json; no repository LICENSE file. See PROVENANCE.md.
// Internal use only; do not redistribute without legal review.
//
// Deliberately not re-exported: CollapsibleSection, Swatch, TodoListCard,
// IconButton, computeDAGLayout, palette internals, and the removed host bridge.
export type { CanvasHostTheme } from "./hooks";
export { getHostTheme, setHostTheme, useHostTheme } from "./hooks";

export type { Color } from "./tokens";

export type { ChartDataPoint, ChartSeries, ChartTone } from "./chart-primitives";
export { BarChart, LineChart, PieChart } from "./chart-primitives";

export type { DiffLineData } from "./diff-view";
export { DiffStats, DiffView } from "./diff-view";

export { Checkbox, Select, TextArea, TextInput, Toggle } from "./form-primitives";

export type { CalloutTone, StatTone, TableColumnAlign, TableRowTone } from "./ui-primitives";
export {
  Button,
  Callout,
  Card,
  CardBody,
  CardHeader,
  Code,
  Divider,
  Grid,
  H1,
  H2,
  H3,
  Link,
  Pill,
  Row,
  Spacer,
  Stack,
  Stat,
  Table,
  Text,
} from "./ui-primitives";

export type { TodoItem } from "./todo-list";
export { TodoList } from "./todo-list";

export { UsageBar } from "./usage-bar";
