import {
  Callout,
  Card,
  CardBody,
  CardHeader,
  DiffStats,
  DiffView,
  Stat,
  Table,
  TodoList,
  UsageBar,
  type CalloutTone,
  type Color,
  type DiffLineData,
  type StatTone,
  type TableColumnAlign,
  type TableRowTone,
  type TodoItem,
} from "../canvas";
import type { OpenUIRenderProps, TableCell } from "./shared";
import { normalizeTableRows } from "./shared";

export interface CardShape {
  title: string;
  children?: unknown;
  subtitle?: string;
  collapsible?: boolean;
  defaultOpen?: boolean;
}

/** 复合 Card/CardHeader/CardBody 折叠成单一 DSL 条目，避免模型拆坏结构。 */
export function CardView({ props, renderNode }: OpenUIRenderProps<CardShape>) {
  return (
    <Card collapsible={props.collapsible} defaultOpen={props.defaultOpen}>
      <CardHeader trailing={props.subtitle}>{props.title}</CardHeader>
      <CardBody>{renderNode(props.children)}</CardBody>
    </Card>
  );
}

export interface CalloutShape {
  tone?: CalloutTone;
  content: string;
  title?: string;
}

export function CalloutView({ props }: OpenUIRenderProps<CalloutShape>) {
  return (
    <Callout tone={props.tone} title={props.title}>
      {props.content}
    </Callout>
  );
}

export interface StatShape {
  value: string | number;
  label: string;
  tone?: StatTone;
}

export function StatView({ props }: OpenUIRenderProps<StatShape>) {
  return <Stat value={props.value} label={props.label} tone={props.tone} />;
}

export interface UsageBarShape {
  segments: Array<{ id: string; value: number; color?: Color }>;
  total: number;
  topLeftLabel?: string;
  topRightLabel?: string;
}

export function UsageBarView({ props }: OpenUIRenderProps<UsageBarShape>) {
  return (
    <UsageBar
      segments={props.segments}
      total={props.total}
      topLeftLabel={props.topLeftLabel}
      topRightLabel={props.topRightLabel}
    />
  );
}

export interface DiffStatsShape {
  additions: number;
  deletions: number;
}

export function DiffStatsView({ props }: OpenUIRenderProps<DiffStatsShape>) {
  return <DiffStats additions={props.additions} deletions={props.deletions} />;
}

export interface DiffViewShape {
  lines: DiffLineData[];
  path?: string;
  language?: string;
  showLineNumbers?: boolean;
  showAccentStrip?: boolean;
}

export function DiffViewView({ props }: OpenUIRenderProps<DiffViewShape>) {
  return (
    <DiffView
      lines={props.lines}
      path={props.path}
      language={props.language}
      showLineNumbers={props.showLineNumbers}
      showAccentStrip={props.showAccentStrip}
    />
  );
}

export interface TodoListShape {
  todos: TodoItem[];
}

export function TodoListView({ props }: OpenUIRenderProps<TodoListShape>) {
  return <TodoList todos={props.todos} />;
}

export interface TableShape {
  headers: string[];
  rows: TableCell[][];
  columnAlign?: TableColumnAlign[];
  rowTone?: TableRowTone[];
  striped?: boolean;
  framed?: boolean;
  emptyMessage?: string;
}

export function TableView({ props }: OpenUIRenderProps<TableShape>) {
  return (
    <Table
      headers={props.headers}
      rows={normalizeTableRows(props.headers, props.rows)}
      columnAlign={props.columnAlign}
      rowTone={props.rowTone}
      striped={props.striped}
      framed={props.framed}
      emptyMessage={props.emptyMessage}
    />
  );
}
