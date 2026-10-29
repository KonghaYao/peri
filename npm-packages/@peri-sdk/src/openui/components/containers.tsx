import {
  Callout,
  Card,
  CardBody,
  CardHeader,
  DiffStats,
  DiffView,
  Gauge,
  Link,
  Stat,
  Table,
  TodoList,
  UsageBar,
  useHostTheme,
  type CalloutTone,
  type Color,
  type DiffLineData,
  type GaugeTone,
  type StatTone,
  type TableColumnAlign,
  type TableRowTone,
  type TodoItem,
} from "../canvas";
import type { OpenUIRenderProps, TableCell } from "./shared";
import { normalizeTableRows, safeExternalUrl } from "./shared";

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

export interface GaugeShape {
  value: number;
  label: string;
  max?: number;
  unit?: string;
  detail?: string;
  tone?: GaugeTone;
  size?: number;
}

export function GaugeView({ props }: OpenUIRenderProps<GaugeShape>) {
  return (
    <Gauge
      value={props.value}
      label={props.label}
      max={props.max}
      unit={props.unit}
      detail={props.detail}
      tone={props.tone}
      size={props.size}
    />
  );
}

export interface DiffStatsShape {
  additions: number;
  deletions: number;
  layout?: "aligned" | "inline";
  showParentheses?: boolean;
}

export function DiffStatsView({ props }: OpenUIRenderProps<DiffStatsShape>) {
  return (
    <DiffStats
      additions={props.additions}
      deletions={props.deletions}
      layout={props.layout}
      showParentheses={props.showParentheses}
    />
  );
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

export type ChangedFileStatus = "added" | "modified" | "deleted" | "renamed";

export interface ChangedFileShape {
  path: string;
  additions?: number;
  deletions?: number;
  status?: ChangedFileStatus;
}

export interface ChangedFilesShape {
  files: ChangedFileShape[];
  title?: string;
  maxRows?: number;
}

const DEFAULT_CHANGED_FILE_ROWS = 50;
const MAX_CHANGED_FILE_ROWS = 200;

const changedFileStatusColors: Record<ChangedFileStatus, Color> = {
  added: "green",
  modified: "blue",
  deleted: "red",
  renamed: "purple",
};

function normalizeChangedPath(path: string): string {
  return path.replace(/\\/g, "/");
}

function changedPathDirectory(path: string): string {
  const index = path.lastIndexOf("/");
  return index === -1 ? "" : path.slice(0, index);
}

function resolveChangedFileRows(value: number | undefined): number {
  if (value === undefined || !Number.isFinite(value)) return DEFAULT_CHANGED_FILE_ROWS;
  return Math.min(MAX_CHANGED_FILE_ROWS, Math.max(1, Math.trunc(value)));
}

function safeCount(value: number | undefined): number {
  return value !== undefined && Number.isFinite(value) ? Math.max(0, Math.trunc(value)) : 0;
}

interface ChangedFilesGroup {
  directory: string;
  rows: Array<{ path: string; file: ChangedFileShape }>;
}

/** 保留首次出现的顺序；路径只做分隔符归一化，不改大小写。 */
function groupChangedFiles(files: readonly ChangedFileShape[]): ChangedFilesGroup[] {
  const groups = new Map<string, ChangedFilesGroup>();
  for (const file of files) {
    const path = normalizeChangedPath(file.path) || "(unknown path)";
    const directory = changedPathDirectory(path);
    const group = groups.get(directory) ?? { directory, rows: [] };
    group.rows.push({ path, file });
    groups.set(directory, group);
  }
  return [...groups.values()];
}

export function ChangedFilesView({ props }: OpenUIRenderProps<ChangedFilesShape>) {
  const { tokens } = useHostTheme();
  const limit = resolveChangedFileRows(props.maxRows);
  const visibleFiles = props.files.slice(0, limit);
  const groups = groupChangedFiles(visibleFiles);
  const hiddenCount = props.files.length - visibleFiles.length;
  const totalAdditions = props.files.reduce((sum, file) => sum + safeCount(file.additions), 0);
  const totalDeletions = props.files.reduce((sum, file) => sum + safeCount(file.deletions), 0);
  const countLabel = `${props.files.length} changed file${props.files.length === 1 ? "" : "s"}`;

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        minWidth: 0,
        boxSizing: "border-box",
        border: `1px solid ${tokens.stroke.secondary}`,
        borderRadius: 8,
        overflow: "hidden",
        background: tokens.fill.quaternary,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "baseline",
          flexWrap: "wrap",
          gap: "4px 8px",
          minWidth: 0,
          padding: "8px 12px",
          borderBottom: `1px solid ${tokens.stroke.tertiary}`,
        }}
      >
        {props.title ? (
          <span style={{ fontWeight: 600, color: tokens.text.primary, overflowWrap: "anywhere" }}>
            {props.title}
          </span>
        ) : null}
        <span style={{ color: tokens.text.secondary, fontSize: 12, lineHeight: "16px" }}>
          {countLabel}
        </span>
        <DiffStats additions={totalAdditions} deletions={totalDeletions} layout="inline" />
      </div>
      {props.files.length === 0 ? (
        <div style={{ padding: "8px 12px", color: tokens.text.tertiary, fontSize: 12, lineHeight: "16px" }}>
          No file changes
        </div>
      ) : (
        <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
          {groups.map((group) => (
            <div key={group.directory || "__root__"} style={{ minWidth: 0 }}>
              <div
                style={{
                  padding: "6px 12px 2px",
                  color: tokens.text.tertiary,
                  fontSize: 11,
                  lineHeight: "16px",
                  fontWeight: 500,
                  overflowWrap: "anywhere",
                }}
              >
                {group.directory === "" ? "/" : `${group.directory}/`}
              </div>
              {group.rows.map(({ path, file }, index) => (
                <div
                  key={`${path}-${index}`}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 8,
                    minWidth: 0,
                    padding: "3px 12px",
                  }}
                >
                  <span
                    title={path}
                    style={{
                      flex: 1,
                      minWidth: 0,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      color: tokens.text.primary,
                      fontSize: 12,
                      lineHeight: "18px",
                    }}
                  >
                    {path}
                  </span>
                  {file.status ? (
                    <span
                      style={{
                        flexShrink: 0,
                        padding: "0 6px",
                        border: `1px solid ${tokens.stroke.tertiary}`,
                        borderRadius: 999,
                        background: tokens.fill.quaternary,
                        color: tokens.category[changedFileStatusColors[file.status]],
                        fontSize: 10,
                        lineHeight: "16px",
                      }}
                    >
                      {file.status}
                    </span>
                  ) : null}
                  <DiffStats
                    additions={file.additions}
                    deletions={file.deletions}
                    layout="aligned"
                  />
                </div>
              ))}
            </div>
          ))}
          {hiddenCount > 0 ? (
            <div
              style={{
                padding: "6px 12px",
                borderTop: `1px solid ${tokens.stroke.tertiary}`,
                color: tokens.text.tertiary,
                fontSize: 11,
                lineHeight: "16px",
              }}
            >
              +{hiddenCount} more files
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}

export interface SourceItemShape {
  title: string;
  url: string;
  snippet?: string;
}

export interface SourcesShape {
  items: SourceItemShape[];
  title?: string;
}

const BLOCKED_URL_HINT =
  "Blocked URL: only plain http(s) URLs up to 2048 characters are allowed.";

export function SourcesView({ props }: OpenUIRenderProps<SourcesShape>) {
  const { tokens } = useHostTheme();
  const heading = props.title?.trim() || "Sources";

  return (
    <section
      aria-label={heading}
      style={{ display: "flex", flexDirection: "column", gap: 8, minWidth: 0 }}
    >
      <div style={{ color: tokens.text.primary, fontSize: 13, lineHeight: "18px", fontWeight: 600 }}>
        {heading}
      </div>
      {props.items.length === 0 ? (
        <div style={{ color: tokens.text.tertiary, fontSize: 12, lineHeight: "16px" }}>
          No sources
        </div>
      ) : (
        <ol
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 6,
            margin: 0,
            padding: 0,
            listStyle: "none",
            minWidth: 0,
          }}
        >
          {props.items.map((item, index) => {
            const href = safeExternalUrl(item.url);
            const hostname = href === undefined ? undefined : new URL(href).hostname;
            const label = item.title.trim() || hostname || "Untitled source";
            const snippet = item.snippet?.trim();
            return (
              <li
                key={`${item.url}-${index}`}
                style={{ display: "flex", alignItems: "flex-start", gap: 8, minWidth: 0 }}
              >
                <span
                  aria-hidden="true"
                  style={{
                    flexShrink: 0,
                    minWidth: "1.5em",
                    textAlign: "right",
                    color: tokens.text.tertiary,
                    fontSize: 12,
                    lineHeight: "18px",
                    fontVariantNumeric: "tabular-nums",
                  }}
                >
                  {index + 1}.
                </span>
                <div
                  style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: 2,
                    flex: 1,
                    minWidth: 0,
                  }}
                >
                  <div
                    style={{
                      display: "flex",
                      alignItems: "baseline",
                      flexWrap: "wrap",
                      gap: 6,
                      minWidth: 0,
                    }}
                  >
                    {href === undefined ? (
                      <span
                        data-blocked-url="true"
                        title={BLOCKED_URL_HINT}
                        style={{
                          color: tokens.text.primary,
                          fontSize: 13,
                          lineHeight: "18px",
                          overflowWrap: "anywhere",
                        }}
                      >
                        {label}
                      </span>
                    ) : (
                      <Link href={href} style={{ overflowWrap: "anywhere" }}>
                        {label}
                      </Link>
                    )}
                    {hostname !== undefined && hostname !== label ? (
                      <span
                        style={{
                          color: tokens.text.tertiary,
                          fontSize: 11,
                          lineHeight: "16px",
                          overflowWrap: "anywhere",
                        }}
                      >
                        {hostname}
                      </span>
                    ) : href === undefined ? (
                      <span
                        style={{
                          color: tokens.category.orange,
                          fontSize: 11,
                          lineHeight: "16px",
                        }}
                      >
                        invalid URL
                      </span>
                    ) : null}
                  </div>
                  {snippet ? (
                    <div
                      style={{
                        color: tokens.text.secondary,
                        fontSize: 12,
                        lineHeight: "16px",
                        overflowWrap: "anywhere",
                      }}
                    >
                      {snippet}
                    </div>
                  ) : null}
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </section>
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
