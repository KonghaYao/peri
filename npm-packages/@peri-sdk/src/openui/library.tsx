// 必须先于 @openuidev/react-lang 求值，关闭开发期 devtools 自动挂载。
import "./devtools-off";
import { action, createLibrary, defineComponent } from "@openuidev/react-lang";
import type { SystemPromptOptions } from "@openuidev/lang-core";
import { z } from "zod";
import { BarChartView, LineChartView, PieChartView } from "./components/charts";
import {
  CalloutView,
  CardView,
  ChangedFilesView,
  DiffStatsView,
  DiffViewView,
  GaugeView,
  SourcesView,
  StatView,
  TableView,
  TodoListView,
  UsageBarView,
} from "./components/containers";
import {
  ButtonView,
  CheckboxView,
  SelectView,
  TextAreaView,
  TextInputView,
  ToggleView,
} from "./components/forms";
import {
  CodeView,
  DividerView,
  GridView,
  H1View,
  H2View,
  H3View,
  LinkView,
  PillView,
  RowView,
  SpacerView,
  StackView,
  TextView,
} from "./components/layout";

/**
 * 唯一事实源：组件名、zod props、description、分组、渲染器与提示词选项。
 * `src/openui/spec.json` 由 `scripts/generate-openui-spec.ts` 从这里生成；服务端只读 `spec.json`
 * （经 `@peri-code/sdk/openui/prompt`），不 import 本文件或 React。
 *
 * children 采用有向无环的三层 union（见 `leafRefs` / `sectionRefs`）：
 * Stack -> Grid | Card | Row | Leaf；Grid -> Card | Row | Leaf；
 * Card -> Row | Leaf；Row -> Leaf。没有 `z.lazy` 递归。
 */

const gapSchema = z.number().int().min(0).max(48).optional();
const alignSchema = z.enum(["start", "center", "end", "stretch"]).optional();
const toneSchema = z.enum(["success", "danger", "warning", "info"]).optional();
const calloutToneSchema = z.enum(["info", "success", "warning", "danger", "neutral"]).optional();

// ---------- Layout & text leaves ----------

const Text = defineComponent({
  name: "Text",
  description:
    "Body text. args: content, tone?(primary|secondary|tertiary|quaternary), size?(body|small), weight?(normal|medium|semibold|bold), italic?, truncate?.",
  props: z.object({
    content: z.union([z.string(), z.number()]),
    tone: z.enum(["primary", "secondary", "tertiary", "quaternary"]).optional(),
    size: z.enum(["body", "small"]).optional(),
    weight: z.enum(["normal", "medium", "semibold", "bold"]).optional(),
    italic: z.boolean().optional(),
    truncate: z.boolean().optional(),
  }),
  component: TextView,
});

const H1 = defineComponent({
  name: "H1",
  description: "Level-1 heading. arg: content.",
  props: z.object({ content: z.string() }),
  component: H1View,
});

const H2 = defineComponent({
  name: "H2",
  description: "Level-2 heading. arg: content.",
  props: z.object({ content: z.string() }),
  component: H2View,
});

const H3 = defineComponent({
  name: "H3",
  description: "Level-3 heading. arg: content.",
  props: z.object({ content: z.string() }),
  component: H3View,
});

const Code = defineComponent({
  name: "Code",
  description: "Inline code span. arg: content. Use Markdown fenced blocks for multi-line code.",
  props: z.object({ content: z.string() }),
  component: CodeView,
});

const Link = defineComponent({
  name: "Link",
  description: "Hyperlink to an http(s) URL. args: label, href. Non-http(s) URLs are not rendered as links.",
  props: z.object({ label: z.string(), href: z.string() }),
  component: LinkView,
});

const Pill = defineComponent({
  name: "Pill",
  description: "Compact status or label chip. args: label, size?(sm|md).",
  props: z.object({ label: z.string(), size: z.enum(["sm", "md"]).optional() }),
  component: PillView,
});

const Spacer = defineComponent({
  name: "Spacer",
  description: "Flexible filler inside Row or Stack. No props.",
  props: z.object({}),
  component: SpacerView,
});

const Divider = defineComponent({
  name: "Divider",
  description: "Horizontal separator line. No props.",
  props: z.object({}),
  component: DividerView,
});

// ---------- Container & data leaves ----------

const Callout = defineComponent({
  name: "Callout",
  description:
    "Highlighted note. args: tone?(info|success|warning|danger|neutral), content, title?.",
  props: z.object({
    tone: calloutToneSchema,
    content: z.string(),
    title: z.string().optional(),
  }),
  component: CalloutView,
});

const Stat = defineComponent({
  name: "Stat",
  description: "Single metric. args: value(string|number), label, tone?(success|danger|warning|info).",
  props: z.object({
    value: z.union([z.string(), z.number()]),
    label: z.string(),
    tone: toneSchema,
  }),
  component: StatView,
});

const UsageBar = defineComponent({
  name: "UsageBar",
  description:
    "Segmented usage bar. args: segments(id,value,color?), total, topLeftLabel?, topRightLabel?. color: gray|purple|green|yellow|cyan|pink|blue|orange|red.",
  props: z.object({
    segments: z.array(
      z.object({
        id: z.string(),
        value: z.number(),
        color: z
          .enum(["gray", "purple", "green", "yellow", "cyan", "pink", "blue", "orange", "red"])
          .optional(),
      }),
    ),
    total: z.number(),
    topLeftLabel: z.string().optional(),
    topRightLabel: z.string().optional(),
  }),
  component: UsageBarView,
});

const Gauge = defineComponent({
  name: "Gauge",
  description:
    "Single-value ring gauge for usage or completion. args: value, label, max?(default 100, must be >0), unit?, detail?, tone?(success|danger|warning|info), size?(72-160, default 96). Display-only; use UsageBar for multi-segment composition and Stat for plain metrics.",
  props: z.object({
    value: z.number(),
    label: z.string(),
    max: z.number().optional(),
    unit: z.string().optional(),
    detail: z.string().optional(),
    tone: toneSchema,
    size: z.number().int().min(72).max(160).optional(),
  }),
  component: GaugeView,
});

const DiffStats = defineComponent({
  name: "DiffStats",
  description:
    "Inline or aligned +additions/-deletions counts. args: additions, deletions, layout?(inline|aligned, default inline), showParentheses?. Counts >=1000 use compact k/m/b notation.",
  props: z.object({
    additions: z.number(),
    deletions: z.number(),
    layout: z.enum(["aligned", "inline"]).optional(),
    showParentheses: z.boolean().optional(),
  }),
  component: DiffStatsView,
});

const DiffView = defineComponent({
  name: "DiffView",
  description:
    "Unified diff. args: lines(type added|removed|unchanged, content, lineNumber?), path?, language?, showLineNumbers?, showAccentStrip?.",
  props: z.object({
    lines: z.array(
      z.object({
        type: z.enum(["added", "removed", "unchanged"]),
        content: z.string(),
        lineNumber: z.number().int().optional(),
      }),
    ),
    path: z.string().optional(),
    language: z.string().optional(),
    showLineNumbers: z.boolean().optional(),
    showAccentStrip: z.boolean().optional(),
  }),
  component: DiffViewView,
});

const ChangedFiles = defineComponent({
  name: "ChangedFiles",
  description:
    "Changed-files summary grouped by directory; display-only, not authoritative. args: files(path, additions?, deletions?, status?: added|modified|deleted|renamed)[], title?, maxRows?(1-200, default 50). Paths use '/'; excess rows collapse to '+N more files'.",
  props: z.object({
    files: z.array(
      z.object({
        path: z.string(),
        additions: z.number().optional(),
        deletions: z.number().optional(),
        status: z.enum(["added", "modified", "deleted", "renamed"]).optional(),
      }),
    ),
    title: z.string().optional(),
    maxRows: z.number().int().min(1).max(200).optional(),
  }),
  component: ChangedFilesView,
});

const Sources = defineComponent({
  name: "Sources",
  description:
    "Ordered source list (1-10 items). args: items(title, url, snippet?)[], title?(default Sources). Only plain http(s) URLs without credentials are linked; rejected URLs render as plain text.",
  props: z.object({
    items: z
      .array(
        z.object({
          title: z.string(),
          url: z.string(),
          snippet: z.string().optional(),
        }),
      )
      .min(1)
      .max(10),
    title: z.string().optional(),
  }),
  component: SourcesView,
});

const TodoList = defineComponent({
  name: "TodoList",
  description:
    "Compact todo list. args: todos(id, content, status: pending|in_progress|completed|cancelled).",
  props: z.object({
    todos: z.array(
      z.object({
        id: z.string(),
        content: z.string(),
        status: z.enum(["pending", "in_progress", "completed", "cancelled"]),
      }),
    ),
  }),
  component: TodoListView,
});

const Table = defineComponent({
  name: "Table",
  description:
    "Data table. args: headers(string[]), rows((string|number)[][]), columnAlign?, rowTone?, striped?, framed?, emptyMessage?. Rows are truncated/padded to headers length.",
  props: z.object({
    headers: z.array(z.string().min(1)).min(1),
    rows: z.array(z.array(z.union([z.string(), z.number()]))),
    columnAlign: z.array(z.enum(["left", "center", "right"])).optional(),
    rowTone: z.array(z.enum(["success", "danger", "warning", "info", "neutral"])).optional(),
    striped: z.boolean().optional(),
    framed: z.boolean().optional(),
    emptyMessage: z.string().optional(),
  }),
  component: TableView,
});

// ---------- Chart leaves ----------

const chartTone = z.enum(["success", "danger", "warning", "info", "neutral"]).optional();
const chartSeries = z.object({
  name: z.string(),
  data: z.array(z.number()),
  tone: chartTone,
});

const BarChart = defineComponent({
  name: "BarChart",
  description:
    "Bar chart. args: categories(string[]), series(name, data:number[], tone?), height?(120-600), stacked?, horizontal?, normalized?, valuePrefix?, valueSuffix?, showValues?. Every series.data length must equal categories length.",
  props: z.object({
    categories: z.array(z.string()).min(1),
    series: z.array(chartSeries).min(1),
    height: z.number().int().min(120).max(600).optional(),
    stacked: z.boolean().optional(),
    horizontal: z.boolean().optional(),
    normalized: z.boolean().optional(),
    valuePrefix: z.string().optional(),
    valueSuffix: z.string().optional(),
    showValues: z.boolean().optional(),
  }),
  component: BarChartView,
});

const LineChart = defineComponent({
  name: "LineChart",
  description:
    "Line chart. args: categories(string[]), series(name, data:number[], tone?), height?(120-600), fill?, valuePrefix?, valueSuffix?, showValues?. Every series.data length must equal categories length.",
  props: z.object({
    categories: z.array(z.string()).min(1),
    series: z.array(chartSeries).min(1),
    height: z.number().int().min(120).max(600).optional(),
    fill: z.boolean().optional(),
    valuePrefix: z.string().optional(),
    valueSuffix: z.string().optional(),
    showValues: z.boolean().optional(),
  }),
  component: LineChartView,
});

const PieChart = defineComponent({
  name: "PieChart",
  description:
    "Pie or donut chart. args: data(label, value, tone?), size?(100-400), donut?. Non-finite values are dropped.",
  props: z.object({
    data: z.array(z.object({ label: z.string(), value: z.number(), tone: chartTone })).min(1),
    size: z.number().int().min(100).max(400).optional(),
    donut: z.boolean().optional(),
  }),
  component: PieChartView,
});

// ---------- Form & action leaves ----------

const TextInput = defineComponent({
  name: "TextInput",
  description:
    "Single-line input bound to a form field. args: name, label?, placeholder?, type?(text|email|url|search|number), defaultValue?.",
  props: z.object({
    name: z.string().min(1),
    label: z.string().optional(),
    placeholder: z.string().optional(),
    type: z.enum(["text", "email", "url", "search", "number"]).optional(),
    defaultValue: z.string().optional(),
  }),
  component: TextInputView,
});

const TextArea = defineComponent({
  name: "TextArea",
  description: "Multi-line input bound to a form field. args: name, label?, placeholder?, rows?, defaultValue?.",
  props: z.object({
    name: z.string().min(1),
    label: z.string().optional(),
    placeholder: z.string().optional(),
    rows: z.number().int().min(1).max(12).optional(),
    defaultValue: z.string().optional(),
  }),
  component: TextAreaView,
});

const Checkbox = defineComponent({
  name: "Checkbox",
  description: "Boolean checkbox bound to a form field. args: name, label, defaultValue?.",
  props: z.object({
    name: z.string().min(1),
    label: z.string(),
    defaultValue: z.boolean().optional(),
  }),
  component: CheckboxView,
});

const Toggle = defineComponent({
  name: "Toggle",
  description: "Boolean switch bound to a form field. args: name, label?, defaultValue?.",
  props: z.object({
    name: z.string().min(1),
    label: z.string().optional(),
    defaultValue: z.boolean().optional(),
  }),
  component: ToggleView,
});

const Select = defineComponent({
  name: "Select",
  description:
    "Dropdown bound to a form field. args: name, options(value,label)[], label?, placeholder?, defaultValue?.",
  props: z.object({
    name: z.string().min(1),
    options: z.array(z.object({ value: z.string(), label: z.string() })).min(1),
    label: z.string().optional(),
    placeholder: z.string().optional(),
    defaultValue: z.string().optional(),
  }),
  component: SelectView,
});

const Button = defineComponent({
  name: "Button",
  description:
    "Action button. args: label, action(@ToAssistant(\"...\") or @OpenUrl(\"...\")), variant?(primary|secondary|ghost), disabled?.",
  props: z.object({
    label: z.string(),
    action: action(),
    variant: z.enum(["primary", "secondary", "ghost"]).optional(),
    disabled: z.boolean().optional(),
  }),
  component: ButtonView,
});

const leafRefs = [
  Text.ref,
  H1.ref,
  H2.ref,
  H3.ref,
  Code.ref,
  Link.ref,
  Pill.ref,
  Spacer.ref,
  Divider.ref,
  Callout.ref,
  Stat.ref,
  UsageBar.ref,
  Gauge.ref,
  DiffStats.ref,
  DiffView.ref,
  ChangedFiles.ref,
  Sources.ref,
  TodoList.ref,
  Table.ref,
  BarChart.ref,
  LineChart.ref,
  PieChart.ref,
  TextInput.ref,
  TextArea.ref,
  Checkbox.ref,
  Toggle.ref,
  Select.ref,
  Button.ref,
] as const;

const Row = defineComponent({
  name: "Row",
  description:
    "Horizontal layout. Children must be leaf components (no Card or Grid). args: children, gap?, align?, justify?, wrap?.",
  props: z.object({
    children: z.array(z.union(leafRefs)),
    gap: gapSchema,
    align: alignSchema,
    justify: z.enum(["start", "center", "end", "space-between"]).optional(),
    wrap: z.boolean().optional(),
  }),
  component: RowView,
});

const Card = defineComponent({
  name: "Card",
  description:
    "Bordered card with header and body; optional collapse. args: title, children(Row or leaf), subtitle?, collapsible?, defaultOpen?.",
  props: z.object({
    title: z.string(),
    children: z.array(z.union([Row.ref, ...leafRefs])),
    subtitle: z.string().optional(),
    collapsible: z.boolean().optional(),
    defaultOpen: z.boolean().optional(),
  }),
  component: CardView,
});

const sectionRefs = [Card.ref, Row.ref, ...leafRefs] as const;

const Grid = defineComponent({
  name: "Grid",
  description:
    "N-column grid; only allowed directly under Stack. args: columns(1-6), children(Card, Row or leaf), gap?, align?.",
  props: z.object({
    columns: z.number().int().min(1).max(6),
    children: z.array(z.union(sectionRefs)),
    gap: gapSchema,
    align: alignSchema,
  }),
  component: GridView,
});

const Stack = defineComponent({
  name: "Stack",
  description:
    "Vertical page root; use as `root = Stack([...])`. args: children(Grid, Card, Row or leaf), gap?.",
  props: z.object({
    children: z.array(z.union([Grid.ref, ...sectionRefs])),
    gap: gapSchema,
  }),
  component: StackView,
});

export const openuiComponentGroups = [
  {
    name: "Layout & text",
    components: ["Stack", "Row", "Grid", "Spacer", "Divider", "Text", "H1", "H2", "H3", "Code", "Link", "Pill"],
    notes: [
      "Stack is the only root. Grid may appear only as a direct child of Stack.",
      "Row children must be leaves; Card children may be Row or leaves.",
    ],
  },
  {
    name: "Containers & data",
    components: [
      "Card",
      "Callout",
      "Stat",
      "UsageBar",
      "Gauge",
      "DiffStats",
      "DiffView",
      "ChangedFiles",
      "Sources",
      "TodoList",
      "Table",
    ],
    notes: ["Use Card for grouping; do not nest Cards."],
  },
  {
    name: "Charts",
    components: ["BarChart", "LineChart", "PieChart"],
    notes: ["All series must have exactly one value per category; mismatches are shown as a warning instead of a chart."],
  },
  {
    name: "Forms & actions",
    components: ["TextInput", "TextArea", "Checkbox", "Toggle", "Select", "Button"],
    notes: ["Form values are ephemeral; place a Button with @ToAssistant in the same Card to submit."],
  },
];

export const openuiPromptOptions = {
  preamble: `You are Peri's assistant. Answer in ordinary Markdown by default.
When a compact structured visual (metrics, comparison, chart, status card, todo list, or a small form) is clearly better than prose, you may emit ONE complete OpenUI Lang program as a fenced code block tagged \`openui\`.
Never use OpenUI for code, shell commands, file paths, or ordinary lists.`,
  additionalRules: [
    "Every openui block must be a complete standalone program, never a patch or only changed statements.",
    "The first line of every program is `root = Stack([...])`.",
    "Put explanation outside the fence; inside the fence there is only OpenUI Lang.",
    "Use at most one openui block per response.",
    'Form values are ephemeral. Add a Button with `@ToAssistant("...")` to submit them.',
    "When the user asks to see OpenUI source, use a `text` code fence, not an `openui` fence.",
  ],
  examples: [
    `root = Stack([title, metrics, table])
title = H2("本周交付")
metrics = Grid(3, [done, blocked, onTime])
done = Stat("12", "已完成任务", "success")
blocked = Stat("3", "阻塞", "danger")
onTime = Stat("87%", "按期率", "info")
table = Table(["模块", "状态", "负责人"], [["渲染", "完成", "A"], ["回传", "进行中", "B"]])`,
    `root = Stack([title, chart, actions])
title = H3("近 7 天请求量")
chart = LineChart(["周一", "周二", "周三", "周四", "周五", "周六", "周日"], [{ name: "请求", data: [120, 160, 140, 200, 180, 240, 220] }])
actions = Row([more], 8, "end", "end")
more = Button("继续分析", @ToAssistant("继续分析这张图"), "secondary")`,
    `root = Stack([gauge])
gauge = Gauge(72, "上下文用量", 100, "%", "144k / 200k tokens", "info")`,
    `root = Stack([changes])
changes = ChangedFiles([{ path: "src/library.tsx", additions: 12, deletions: 3, status: "modified" }, { path: "README.md", additions: 2 }], "本次改动", 20)`,
    `root = Stack([sources])
sources = Sources([{ title: "Peri 文档", url: "https://example.com/peri", snippet: "Agent loop overview" }])`,
  ],
  inlineMode: false,
  editMode: false,
  toolCalls: false,
  bindings: false,
  builtinFunctions: false,
} satisfies SystemPromptOptions;

export const openuiLibrary = createLibrary({
  id: "peri-openui",
  root: "Stack",
  components: [
    Stack,
    Row,
    Grid,
    Spacer,
    Divider,
    Text,
    H1,
    H2,
    H3,
    Code,
    Link,
    Pill,
    Card,
    Callout,
    Stat,
    UsageBar,
    Gauge,
    DiffStats,
    DiffView,
    ChangedFiles,
    Sources,
    TodoList,
    Table,
    BarChart,
    LineChart,
    PieChart,
    TextInput,
    TextArea,
    Checkbox,
    Toggle,
    Select,
    Button,
  ],
  componentGroups: [...openuiComponentGroups],
});
