import { createContext, useContext, type ReactNode } from "react";
import { Callout } from "../canvas";

/** Renderer 内交互是否可用：流式中或聊天忙时禁用按钮与表单。 */
export const OpenUIActionEnabledContext = createContext(true);

export function useOpenUIActionsEnabled(): boolean {
  return useContext(OpenUIActionEnabledContext);
}

/**
 * 组件 renderer 的最小契约，避免 `components/*` 直接依赖 `@openuidev/react-lang`
 * 的内部类型（真正的 `ComponentRenderProps` 结构与此兼容）。
 */
export interface OpenUIRenderProps<P> {
  readonly props: P;
  readonly renderNode: (value: unknown) => ReactNode;
  readonly statementId?: string;
}

export type TableCell = string | number;

/**
 * 表格单元格不接收 ReactNode：统一转字符串，并按表头长度截断/补空，
 * 避免模型给出错列时由 canvas 渲染出不可控结构。
 */
export function normalizeTableRows(
  headers: readonly string[],
  rows: readonly (readonly TableCell[])[],
): string[][] {
  return rows.map((row) =>
    headers.map((_, index) => {
      const cell = row[index];
      return cell == null ? "" : String(cell);
    }),
  );
}

/** 只允许 http/https；`javascript:`、`data:` 等一律拒绝。 */
export function safeHref(href: string): string | undefined {
  try {
    const url = new URL(href);
    return url.protocol === "http:" || url.protocol === "https:" ? url.toString() : undefined;
  } catch {
    return undefined;
  }
}

export const MAX_EXTERNAL_URL_LENGTH = 2048;

/**
 * 外链来源的独立校验：只允许 http/https，并拒绝 userinfo 与超长 URL。
 * 与 `safeHref` 分开，避免改变 `Link` 的既有行为；非法值不返回原始字符串，
 * 防止把 `user:token@host` 这类凭据回显到 DOM。
 */
export function safeExternalUrl(href: string): string | undefined {
  if (href.length === 0 || href.length > MAX_EXTERNAL_URL_LENGTH) return undefined;
  try {
    const url = new URL(href);
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    if (url.username !== "" || url.password !== "") return undefined;
    if (url.hostname === "") return undefined;
    return url.toString();
  } catch {
    return undefined;
  }
}

export function chartDataMismatch(series: readonly { data: readonly number[] }[], categories: readonly string[]): boolean {
  return series.some((entry) => entry.data.length !== categories.length);
}

export interface ChartWarningProps {
  readonly message?: string;
}

/** 图表数据不完整时显式暴露问题，不伪造 0 值。 */
export function ChartWarning({
  message = "分类数与数值数量不一致，已省略该图。",
}: ChartWarningProps): ReactNode {
  return (
    <Callout tone="warning" title="图表数据不完整">
      {message}
    </Callout>
  );
}
