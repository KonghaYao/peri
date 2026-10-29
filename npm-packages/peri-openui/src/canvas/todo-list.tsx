// Vendored from peri-canvas@0.1.0 (packages/canvas/src/todo-list.tsx).
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// Internal use only; do not redistribute without legal review.
import type { CSSProperties, JSX } from "react";
import { useHostTheme } from "./hooks";
import { mergeStyle } from "./merge-style";
import { canvasSpacing } from "./theme";

export type TodoStatus = "pending" | "in_progress" | "completed" | "cancelled";

export interface TodoItem {
  readonly id: string;
  readonly content: string;
  readonly status: TodoStatus;
}

export type TodoListProps = {
  todos: readonly TodoItem[];
  dimmedTodoIds?: ReadonlySet<string>;
  onTodoClick?: (todo: TodoItem) => void;
  style?: CSSProperties;
};

function asTodoStatus(status: string): TodoStatus {
  switch (status) {
    case "in_progress":
    case "completed":
    case "cancelled":
    case "pending":
      return status;
    default:
      return "pending";
  }
}

const todoLineHeight = "18px";
const todoFontSize = "13px";
const todoStatusAriaLabel: Record<TodoStatus, string> = {
  pending: "Pending",
  in_progress: "In progress",
  completed: "Completed",
  cancelled: "Cancelled",
};

function TodoStatusGlyph({ status, color }: { status: TodoStatus; color: string }) {
  const style = { display: "block" as const, flexShrink: 0, color };
  const label = todoStatusAriaLabel[status];
  const stroke = {
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (status) {
    case "pending":
      return (
        <svg width={14} height={14} viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" role="img" aria-label={label} style={style}>
          <path d="M10.1 2.182a10 10 0 0 1 3.8 0" {...stroke} />
          <path d="M13.9 21.818a10 10 0 0 1-3.8 0" {...stroke} />
          <path d="M17.609 3.721a10 10 0 0 1 2.69 2.7" {...stroke} />
          <path d="M2.182 13.9a10 10 0 0 1 0-3.8" {...stroke} />
          <path d="M20.279 17.609a10 10 0 0 1-2.7 2.69" {...stroke} />
          <path d="M21.818 10.1a10 10 0 0 1 0 3.8" {...stroke} />
          <path d="M3.721 6.391a10 10 0 0 1 2.7-2.69" {...stroke} />
          <path d="M6.391 20.279a10 10 0 0 1-2.69-2.7" {...stroke} />
        </svg>
      );
    case "in_progress":
      return (
        <svg width={14} height={14} viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg" role="img" aria-label={label} style={style}>
          <circle cx={12} cy={12} r={10} {...stroke} />
          <path d="M8 12h8" {...stroke} />
          <path d="m12 16 4-4-4-4" {...stroke} />
        </svg>
      );
    case "completed":
      return (
        <svg width={14} height={14} viewBox="0 0 14 14" fill="none" xmlns="http://www.w3.org/2000/svg" role="img" aria-label={label} style={style}>
          <circle cx={7} cy={7} r={5.25} stroke="currentColor" strokeWidth={1.2} />
          <path d="M4.5 7 6.5 9 9.5 5" stroke="currentColor" strokeWidth={1.2} strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      );
    case "cancelled":
      return (
        <svg width={14} height={14} viewBox="0 0 14 14" fill="none" xmlns="http://www.w3.org/2000/svg" role="img" aria-label={label} style={style}>
          <circle cx={7} cy={7} r={5.25} stroke="currentColor" strokeWidth={1.2} />
          <path d="m5 5 4 4M9 5l-4 4" stroke="currentColor" strokeWidth={1.2} strokeLinecap="round" />
        </svg>
      );
  }
}

function indicatorColor(
  status: TodoStatus,
  tokens: ReturnType<typeof useHostTheme>["tokens"],
) {
  switch (status) {
    case "in_progress":
      return tokens.text.secondary;
    case "completed":
    case "cancelled":
      return tokens.text.tertiary;
    case "pending":
      return tokens.text.quaternary;
  }
}

function CanvasTodoListItem({
  todo,
  isDimmed,
  onTodoClick,
}: {
  todo: TodoItem;
  isDimmed?: boolean;
  onTodoClick?: (todo: TodoItem) => void;
}) {
  const { tokens } = useHostTheme();
  const status = asTodoStatus(todo.status);
  const color = indicatorColor(status, tokens);
  const done = status === "completed" || status === "cancelled";
  const textColor = done
    ? tokens.text.quaternary
    : status === "in_progress"
      ? isDimmed
        ? tokens.text.secondary
        : tokens.text.primary
      : tokens.text.secondary;
  return (
    <li
      id={`todo-${todo.id}`}
      style={{ position: "relative", display: "flex", alignItems: "flex-start", padding: 0, margin: 0 }}
    >
      <button
        type="button"
        onClick={() => onTodoClick?.(todo)}
        style={{
          all: "unset",
          boxSizing: "border-box",
          display: "flex",
          alignItems: "flex-start",
          gap: `${canvasSpacing[2]}px`,
          flex: 1,
          width: "100%",
          minWidth: 0,
          cursor: "pointer",
          color: tokens.text.secondary,
          font: "inherit",
          textAlign: "left",
        }}
      >
        <span
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
            width: 14,
            minHeight: todoLineHeight,
          }}
        >
          <TodoStatusGlyph status={status} color={color} />
        </span>
        <span
          style={{
            flex: 1,
            minWidth: 0,
            userSelect: "text",
            cursor: "pointer",
            fontSize: todoFontSize,
            lineHeight: todoLineHeight,
            color: textColor,
            textDecoration: done ? "line-through" : undefined,
          }}
        >
          {todo.content}
        </span>
      </button>
    </li>
  );
}

export function TodoList({
  todos = [],
  dimmedTodoIds,
  onTodoClick,
  style,
}: TodoListProps): JSX.Element | null {
  if (todos.length === 0) return null;
  return (
    <ul
      style={mergeStyle(
        {
          margin: 0,
          padding: 0,
          listStyle: "none",
          display: "flex",
          flexDirection: "column",
          gap: `${canvasSpacing[2.5]}px`,
          width: "100%",
        },
        style,
      )}
    >
      {todos.map((todo) => (
        <CanvasTodoListItem
          key={todo.id}
          todo={todo}
          isDimmed={dimmedTodoIds?.has(todo.id)}
          onTodoClick={onTodoClick}
        />
      ))}
    </ul>
  );
}
