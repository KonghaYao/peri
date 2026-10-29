// Vendored from peri-canvas@0.1.0 (packages/canvas/src/ui-primitives.tsx).
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// Internal use only; do not redistribute without legal review.
import {
  createContext,
  isValidElement,
  cloneElement,
  useCallback,
  useContext,
  useState,
  type CSSProperties,
  type JSX,
  type ReactNode,
} from "react";
import { CalloutToneIcon } from "./callout-tone-icons";
import { useHostTheme } from "./hooks";
import { mergeStyle } from "./merge-style";
import { chartPalette } from "./tokens";
import {
  canvasRadius,
  canvasSpacing,
  canvasTypography,
  CANVAS_MONO_FONT_STACK,
} from "./theme";

export { mergeStyle };

const mono = CANVAS_MONO_FONT_STACK;

export type StackProps = {
  children?: ReactNode;
  gap?: number;
  style?: CSSProperties;
};

export function Stack({ children, gap = 12, style }: StackProps): JSX.Element {
  return (
    <div
      style={mergeStyle(
        { display: "flex", flexDirection: "column", gap: `${gap}px`, width: "100%" },
        style,
      )}
    >
      {children}
    </div>
  );
}

export type RowProps = {
  children?: ReactNode;
  gap?: number;
  align?: "start" | "center" | "end" | "stretch";
  justify?: "start" | "center" | "end" | "space-between";
  wrap?: boolean;
  style?: CSSProperties;
};

export function Row({
  children,
  gap = 8,
  align = "center",
  justify = "start",
  wrap = false,
  style,
}: RowProps): JSX.Element {
  return (
    <div
      style={mergeStyle(
        {
          display: "flex",
          flexDirection: "row",
          flexWrap: wrap ? "wrap" : "nowrap",
          alignItems: align,
          justifyContent: justify,
          gap: `${gap}px`,
          width: "100%",
        },
        style,
      )}
    >
      {children}
    </div>
  );
}

export type GridProps = {
  children?: ReactNode;
  columns: number | string;
  gap?: number;
  align?: "start" | "center" | "end" | "stretch";
  style?: CSSProperties;
};

export function Grid({
  children,
  columns,
  gap = 12,
  align = "stretch",
  style,
}: GridProps): JSX.Element {
  const gridTemplateColumns =
    typeof columns === "number" ? `repeat(${columns}, minmax(0, 1fr))` : columns;
  return (
    <div
      style={mergeStyle(
        {
          display: "grid",
          gridTemplateColumns,
          gap: `${gap}px`,
          alignItems: align,
          width: "100%",
        },
        style,
      )}
    >
      {children}
    </div>
  );
}

export type DividerProps = { style?: CSSProperties };

export function Divider({ style }: DividerProps): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <hr
      style={mergeStyle(
        {
          width: "100%",
          border: "none",
          borderTop: `1px solid ${tokens.stroke.tertiary}`,
          margin: 0,
        },
        style,
      )}
    />
  );
}

export function Spacer(): JSX.Element {
  return <div style={{ flex: 1 }} />;
}

export type TableColumnAlign = "left" | "center" | "right";
export type TableRowTone = "success" | "danger" | "warning" | "info" | "neutral";

export type TableProps = {
  headers: ReactNode[];
  rows: ReactNode[][];
  columnAlign?: Array<TableColumnAlign | undefined>;
  rowTone?: Array<TableRowTone | undefined>;
  framed?: boolean;
  striped?: boolean;
  stickyHeader?: boolean;
  style?: CSSProperties;
  emptyMessage?: ReactNode;
};

const tableCellPadding = {
  padding: `${canvasSpacing[2]}px ${canvasSpacing[3]}px`,
};
const rowToneMarkerColors: Record<TableRowTone, string> = {
  success: chartPalette.lightGreen,
  danger: chartPalette.darkAmber,
  warning: chartPalette.brightOrange,
  info: chartPalette.lightBlue,
  neutral: chartPalette.muted,
};
const rowToneMarkerSizePx = canvasSpacing[1.5];
const rowToneMarkerOffsetTopPx =
  (parseInt(canvasTypography.body.lineHeight, 10) - rowToneMarkerSizePx) / 2;

function RowToneMarker({ tone }: { tone: TableRowTone }) {
  return (
    <span
      aria-hidden
      style={{
        width: rowToneMarkerSizePx,
        height: rowToneMarkerSizePx,
        marginTop: rowToneMarkerOffsetTopPx,
        borderRadius: "50%",
        background: rowToneMarkerColors[tone],
        flexShrink: 0,
      }}
    />
  );
}

export function Table({
  headers = [],
  rows = [],
  columnAlign,
  rowTone,
  framed = true,
  striped = false,
  stickyHeader = false,
  style,
  emptyMessage = "No rows.",
}: TableProps): JSX.Element {
  const { tokens } = useHostTheme();
  if (headers.length === 0) {
    return (
      <div
        style={{
          padding: `${canvasSpacing[3]}px`,
          color: tokens.text.secondary,
          fontSize: canvasTypography.body.fontSize,
        }}
      >
        Add at least one header.
      </div>
    );
  }
  const columnCount = headers.length;
  const tableStyle: CSSProperties = {
    minWidth: "100%",
    borderCollapse: stickyHeader ? "separate" : "collapse",
    ...(stickyHeader ? { borderSpacing: 0 } : undefined),
    tableLayout: "auto",
    fontSize: canvasTypography.body.fontSize,
    lineHeight: canvasTypography.body.lineHeight,
    color: tokens.text.primary,
  };
  const alignAt = (index: number): TableColumnAlign =>
    columnAlign?.[index] ?? "left";
  const headerCell = (index: number): CSSProperties => ({
    ...tableCellPadding,
    textAlign: alignAt(index),
    fontWeight: 500,
    color: tokens.text.tertiary,
    borderBottom: `1px solid ${tokens.stroke.tertiary}`,
    ...(stickyHeader
      ? {
          position: "sticky",
          top: 0,
          zIndex: 2,
          backgroundColor: tokens.bg.editor,
        }
      : undefined),
  });
  const bodyCell = (index: number): CSSProperties => ({
    ...tableCellPadding,
    textAlign: alignAt(index),
    verticalAlign: "top",
    color: tokens.text.primary,
  });
  const table = (
    <table style={mergeStyle(tableStyle, style)}>
      <thead>
        <tr>
          {headers.map((header, index) => (
            <th key={index} scope="col" style={headerCell(index)}>
              {header}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.length === 0 ? (
          <tr>
            <td
              colSpan={columnCount}
              style={mergeStyle(bodyCell(0), {
                color: tokens.text.tertiary,
                borderBottom: `1px solid ${tokens.stroke.tertiary}`,
              })}
            >
              {emptyMessage}
            </td>
          </tr>
        ) : (
          rows.map((row, rowIndex) => {
            const tone = rowTone?.[rowIndex];
            return (
              <tr
                key={rowIndex}
                style={{
                  ...(rowIndex < rows.length - 1
                    ? { borderBottom: `1px solid ${tokens.stroke.tertiary}` }
                    : undefined),
                  ...(striped && rowIndex % 2 === 1
                    ? { background: tokens.fill.quaternary }
                    : undefined),
                }}
              >
                {Array.from({ length: columnCount }, (_, colIndex) => {
                  const cell = row[colIndex] ?? null;
                  return (
                    <td key={colIndex} style={bodyCell(colIndex)}>
                      {tone && colIndex === 0 ? (
                        <span
                          style={{
                            display: "inline-flex",
                            alignItems: "flex-start",
                            gap: `${canvasSpacing[1.5]}px`,
                            minWidth: 0,
                            maxWidth: "100%",
                          }}
                        >
                          <RowToneMarker tone={tone} />
                          <span style={{ minWidth: 0, flex: 1 }}>{cell}</span>
                        </span>
                      ) : (
                        cell
                      )}
                    </td>
                  );
                })}
              </tr>
            );
          })
        )}
      </tbody>
    </table>
  );
  if (!framed) return table;
  return (
    <div
      style={{
        width: "100%",
        minWidth: 0,
        boxSizing: "border-box",
        ...(stickyHeader
          ? { height: "100%", maxHeight: "100%", minHeight: 0 }
          : undefined),
        border: `1px solid ${tokens.stroke.tertiary}`,
        borderRadius: `${canvasRadius.lg}px`,
        background: tokens.bg.editor,
        overflowX: "auto",
        overflowY: stickyHeader ? "auto" : "clip",
      }}
    >
      {table}
    </div>
  );
}

const textWeightMap = {
  normal: 400,
  medium: 500,
  semibold: 590,
  bold: 650,
} as const;

const typographyInlineContext = createContext(false);

export type TextWeight = "normal" | "medium" | "semibold" | "bold";

export type TextProps = {
  children?: ReactNode;
  tone?: "primary" | "secondary" | "tertiary" | "quaternary";
  size?: "body" | "small";
  as?: "p" | "span";
  weight?: TextWeight;
  italic?: boolean;
  truncate?: boolean | "start" | "end";
  style?: CSSProperties;
};

export function Text({
  children,
  tone = "primary",
  size = "body",
  as,
  weight = "normal",
  italic = false,
  truncate = false,
  style,
}: TextProps): JSX.Element {
  const { tokens } = useHostTheme();
  const nested = useContext(typographyInlineContext);
  const tag = as ?? (nested ? "span" : "p");
  const colors = {
    primary: tokens.text.primary,
    secondary: tokens.text.secondary,
    tertiary: tokens.text.tertiary,
    quaternary: tokens.text.quaternary,
  };
  const type = size === "small" ? canvasTypography.small : canvasTypography.body;
  const truncateSide = truncate === true ? "end" : truncate === false ? null : truncate;
  const truncateStyle: CSSProperties | undefined =
    truncateSide !== null
      ? {
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          ...(tag === "span" ? { display: "inline-block", maxWidth: "100%" } : {}),
          ...(truncateSide === "start" ? { direction: "rtl", textAlign: "left" } : {}),
        }
      : undefined;
  const content =
    truncateSide === "start" ? <bdi>{children}</bdi> : children;
  const shared: CSSProperties = mergeStyle(
    {
      margin: 0,
      color: colors[tone],
      fontSize: type.fontSize,
      lineHeight: type.lineHeight,
      fontWeight: textWeightMap[weight],
      fontStyle: italic ? "italic" : undefined,
      ...truncateStyle,
    },
    style,
  );
  return (
    <typographyInlineContext.Provider value={true}>
      {tag === "span" ? (
        <span style={shared}>{content}</span>
      ) : (
        <p style={shared}>{content}</p>
      )}
    </typographyInlineContext.Provider>
  );
}

export type H1Props = { children?: ReactNode; style?: CSSProperties };
export type H2Props = { children?: ReactNode; style?: CSSProperties };
export type H3Props = { children?: ReactNode; style?: CSSProperties };

export function H1({ children, style }: H1Props): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <typographyInlineContext.Provider value={true}>
      <h1
        style={mergeStyle(
          {
            margin: 0,
            color: tokens.text.primary,
            fontSize: canvasTypography.h1.fontSize,
            lineHeight: canvasTypography.h1.lineHeight,
            fontWeight: canvasTypography.h1.fontWeight,
          },
          style,
        )}
      >
        {children}
      </h1>
    </typographyInlineContext.Provider>
  );
}

export function H2({ children, style }: H2Props): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <typographyInlineContext.Provider value={true}>
      <h2
        style={mergeStyle(
          {
            margin: 0,
            color: tokens.text.primary,
            fontSize: canvasTypography.h2.fontSize,
            lineHeight: canvasTypography.h2.lineHeight,
            fontWeight: canvasTypography.h2.fontWeight,
          },
          style,
        )}
      >
        {children}
      </h2>
    </typographyInlineContext.Provider>
  );
}

export function H3({ children, style }: H3Props): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <typographyInlineContext.Provider value={true}>
      <h3
        style={mergeStyle(
          {
            margin: 0,
            color: tokens.text.primary,
            fontSize: canvasTypography.h3.fontSize,
            lineHeight: canvasTypography.h3.lineHeight,
            fontWeight: canvasTypography.h3.fontWeight,
          },
          style,
        )}
      >
        {children}
      </h3>
    </typographyInlineContext.Provider>
  );
}

export type CodeProps = { children?: ReactNode; style?: CSSProperties };

export function Code({ children, style }: CodeProps): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <code
      style={mergeStyle(
        {
          fontFamily: mono,
          fontSize: "0.92em",
          padding: "2px 5px",
          borderRadius: `${canvasRadius.sm}px`,
          background: tokens.fill.quaternary,
          color: tokens.text.primary,
        },
        style,
      )}
    >
      {children}
    </code>
  );
}

export type LinkProps = {
  children?: ReactNode;
  href: string;
  style?: CSSProperties;
};

export function Link({ children, href, style }: LinkProps): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      style={mergeStyle(
        {
          color: tokens.text.link,
          textDecoration: "underline",
          textUnderlineOffset: "2px",
          textDecorationColor: `${tokens.text.link}80`,
        },
        style,
      )}
    >
      {children}
    </a>
  );
}

export function CanvasChevron({ expanded }: { expanded: boolean }): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <svg
      width={12}
      height={12}
      viewBox="0 0 12 12"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden
      style={{
        display: "block",
        flexShrink: 0,
        color: tokens.text.tertiary,
        transform: expanded ? undefined : "rotate(-90deg)",
      }}
    >
      <path
        d="M3 4.5 6 7.5 9 4.5"
        stroke="currentColor"
        strokeWidth={1.2}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export type CardSize = "base" | "lg";
export type CardVariant = "default" | "borderless";

type CardChrome = {
  size: CardSize;
  stickyHeader: boolean;
  collapsible: boolean;
  isOpen: boolean;
  toggle: () => void;
};

const defaultCardChrome: CardChrome = {
  size: "base",
  stickyHeader: false,
  collapsible: false,
  isOpen: true,
  toggle: () => {},
};

const CardChromeContext = createContext<CardChrome>(defaultCardChrome);

export type CardProps = {
  children?: ReactNode;
  variant?: CardVariant;
  size?: CardSize;
  stickyHeader?: boolean;
  collapsible?: boolean;
  defaultOpen?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  style?: CSSProperties;
};

export function Card({
  children,
  variant = "default",
  size = "base",
  stickyHeader = false,
  collapsible = false,
  defaultOpen = true,
  open: openProp,
  onOpenChange,
  style,
}: CardProps): JSX.Element {
  const { tokens } = useHostTheme();
  const controlled = openProp !== undefined;
  const [uncontrolledOpen, setUncontrolledOpen] = useState(defaultOpen);
  const isOpen = !collapsible || (controlled ? openProp : uncontrolledOpen);
  const toggle = useCallback(() => {
    if (!collapsible) return;
    const next = !isOpen;
    if (!controlled) setUncontrolledOpen(next);
    onOpenChange?.(next);
  }, [collapsible, isOpen, controlled, onOpenChange]);
  const chrome: CSSProperties = {
    boxSizing: "border-box",
    background: tokens.bg.editor,
    overflow: "clip",
    ...(variant === "default"
      ? {
          border: `1px solid ${tokens.stroke.tertiary}`,
          borderRadius: `${canvasRadius.lg}px`,
        }
      : { border: "none", borderRadius: 0 }),
  };
  return (
    <CardChromeContext.Provider
      value={{ size, stickyHeader, collapsible, isOpen, toggle }}
    >
      <div style={mergeStyle(chrome, style)}>
        <div style={{ boxSizing: "border-box", position: "relative" }}>{children}</div>
      </div>
    </CardChromeContext.Provider>
  );
}

export type CardHeaderProps = {
  children?: ReactNode;
  trailing?: ReactNode;
  style?: CSSProperties;
};

function compactTrailingPills(node: ReactNode): ReactNode {
  if (
    isValidElement<PillProps>(node) &&
    node.type === Pill &&
    node.props.size !== "sm"
  ) {
    return cloneElement(node, { size: "sm" });
  }
  return node;
}

export function CardHeader({
  children,
  trailing,
  style,
}: CardHeaderProps): JSX.Element {
  const { tokens } = useHostTheme();
  const { size, stickyHeader, collapsible, isOpen, toggle } =
    useContext(CardChromeContext);
  const height = size === "lg" ? 32 : 28;
  const padX = size === "lg" ? canvasSpacing[2.5] : canvasSpacing[2];
  const gap = size === "lg" ? canvasSpacing[2] : canvasSpacing[1.5];
  const headerStyle: CSSProperties = {
    boxSizing: "border-box",
    ...(stickyHeader
      ? { position: "sticky", top: 0, zIndex: 5, background: tokens.bg.editor }
      : { position: "relative" }),
    display: "flex",
    alignItems: "center",
    height: `${height}px`,
    fontSize: "12px",
    color: tokens.text.primary,
    borderBottom:
      collapsible && !isOpen ? "none" : `1px solid ${tokens.stroke.tertiary}`,
  };
  const titleStyle: CSSProperties = {
    boxSizing: "border-box",
    flex: 1,
    minWidth: 0,
    display: "flex",
    alignItems: "center",
    height: "100%",
    padding: `0 ${padX}px`,
    gap: `${gap}px`,
    overflow: "hidden",
  };
  const trailingStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    gap: `${canvasSpacing[1.5]}px`,
    paddingRight: `${padX}px`,
    flexShrink: 0,
    fontSize: "11px",
    color: tokens.text.tertiary,
  };
  const trailingNode = trailing != null ? compactTrailingPills(trailing) : null;
  if (collapsible) {
    return (
      <button
        type="button"
        onClick={toggle}
        aria-expanded={isOpen}
        style={mergeStyle(
          { all: "unset", ...headerStyle, cursor: "pointer", width: "100%", font: "inherit", color: "inherit" },
          style,
        )}
      >
        <div style={titleStyle}>
          <CanvasChevron expanded={isOpen} />
          {children}
        </div>
        {trailingNode ? <div style={trailingStyle}>{trailingNode}</div> : null}
      </button>
    );
  }
  return (
    <div style={mergeStyle(headerStyle, style)}>
      <div style={titleStyle}>{children}</div>
      {trailingNode ? <div style={trailingStyle}>{trailingNode}</div> : null}
    </div>
  );
}

export type CardBodyProps = {
  children?: ReactNode;
  style?: CSSProperties;
};

export function CardBody({
  children,
  style,
}: CardBodyProps): JSX.Element | null {
  const { tokens } = useHostTheme();
  const { collapsible, isOpen } = useContext(CardChromeContext);
  if (collapsible && !isOpen) return null;
  return (
    <div
      style={mergeStyle(
        {
          boxSizing: "border-box",
          padding: `${canvasSpacing[3]}px`,
          fontSize: canvasTypography.small.fontSize,
          lineHeight: canvasTypography.small.lineHeight,
          color: tokens.text.primary,
        },
        style,
      )}
    >
      {children}
    </div>
  );
}

export type ButtonProps = {
  children?: ReactNode;
  variant?: "primary" | "secondary" | "ghost";
  disabled?: boolean;
  type?: "button" | "submit" | "reset";
  style?: CSSProperties;
  onClick?: () => void;
};

export function Button({
  children,
  variant = "primary",
  disabled = false,
  type = "button",
  style,
  onClick,
}: ButtonProps): JSX.Element {
  const { tokens } = useHostTheme();
  const [hovered, setHovered] = useState(false);
  const base: CSSProperties = {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    gap: `${canvasSpacing[0.5]}px`,
    height: "24px",
    padding: `0 ${canvasSpacing[1.5]}px`,
    borderRadius: `${canvasRadius.md}px`,
    fontSize: canvasTypography.body.fontSize,
    lineHeight: canvasTypography.body.lineHeight,
    fontWeight: 500,
    fontFamily: "inherit",
    cursor: disabled ? "not-allowed" : "pointer",
    border: "1px solid transparent",
    opacity: disabled ? 0.5 : 1,
    width: "auto",
    whiteSpace: "nowrap",
    userSelect: "none",
    boxSizing: "border-box",
  };
  const variants: Record<NonNullable<ButtonProps["variant"]>, CSSProperties> = {
    primary: {
      background:
        hovered && !disabled ? tokens.accent.controlHover : tokens.accent.control,
      color: tokens.text.onAccent,
      borderColor: tokens.accent.control,
    },
    secondary: {
      background: tokens.fill.secondary,
      color: tokens.text.primary,
      borderColor: tokens.stroke.primary,
    },
    ghost: {
      background:
        hovered && !disabled ? tokens.fill.tertiary : "transparent",
      color: tokens.text.secondary,
      borderColor: "transparent",
    },
  };
  return (
    <button
      type={type}
      disabled={disabled}
      onClick={disabled ? undefined : onClick}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      style={mergeStyle(
        mergeStyle(
          mergeStyle(base, variants[variant]),
          variant === "primary" || variant === "ghost"
            ? { transition: "background 120ms ease" }
            : {},
        ),
        style,
      )}
    >
      {children}
    </button>
  );
}

export type PillSize = "sm" | "md";

export type PillProps = {
  children?: ReactNode;
  active?: boolean;
  size?: PillSize;
  leadingContent?: ReactNode;
  keyboardHint?: string;
  disabled?: boolean;
  title?: string;
  style?: CSSProperties;
  onClick?: () => void;
};

export function Pill({
  children,
  active = false,
  size = "md",
  leadingContent,
  keyboardHint,
  disabled = false,
  title,
  style,
  onClick,
}: PillProps): JSX.Element {
  const { tokens } = useHostTheme();
  const clickable = !!onClick;
  const compact = size === "sm";
  const color = active ? tokens.text.primary : tokens.text.tertiary;
  const border = active ? "transparent" : tokens.stroke.secondary;
  const background = active ? tokens.fill.secondary : "transparent";
  const pillStyle: CSSProperties = {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    boxSizing: "border-box",
    borderRadius: `${canvasRadius.full}px`,
    whiteSpace: "nowrap",
    userSelect: "none",
    fontFamily: "inherit",
    fontWeight: active ? 500 : 400,
    fontSize: compact ? "10px" : "12px",
    lineHeight: compact ? "12px" : "14px",
    background: compact ? tokens.fill.quaternary : background,
    color,
    border: compact ? "none" : `1px solid ${border}`,
    padding: compact
      ? `${canvasSpacing[0.5]}px ${canvasSpacing[1.5]}px`
      : `${canvasSpacing[1.5]}px ${canvasSpacing[2.5]}px`,
    gap: compact ? `${canvasSpacing[1]}px` : `${canvasSpacing[1.5]}px`,
    cursor: clickable ? (disabled ? "not-allowed" : "pointer") : "default",
    opacity: disabled ? 0.3 : 1,
    transition: clickable ? "color 120ms ease, background 120ms ease" : undefined,
  };
  const inner = (
    <>
      {leadingContent ? (
        <span
          style={{
            display: "inline-flex",
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
            color: "inherit",
          }}
        >
          {leadingContent}
        </span>
      ) : null}
      <span style={{ flexShrink: 0, color: "inherit" }}>{children}</span>
      {keyboardHint ? (
        <span style={{ flexShrink: 0, color: tokens.text.primary, opacity: 0.3 }}>
          {keyboardHint}
        </span>
      ) : null}
    </>
  );
  if (clickable) {
    return (
      <button
        type="button"
        disabled={disabled}
        title={title}
        onClick={disabled ? undefined : onClick}
        style={mergeStyle(mergeStyle(pillStyle, { margin: 0 }), style)}
      >
        {inner}
      </button>
    );
  }
  return (
    <span title={title} style={mergeStyle(pillStyle, style)}>
      {inner}
    </span>
  );
}

export type StatTone = "success" | "danger" | "warning" | "info";
export type StatProps = {
  value: ReactNode;
  label: string;
  tone?: StatTone;
  style?: CSSProperties;
};

const statToneColors: Record<StatTone, string> = {
  success: chartPalette.lightGreen,
  danger: chartPalette.darkAmber,
  warning: chartPalette.brightOrange,
  info: chartPalette.lightBlue,
};

export function Stat({ value, label, tone, style }: StatProps): JSX.Element {
  const { tokens } = useHostTheme();
  return (
    <div
      style={mergeStyle(
        {
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: `${canvasSpacing[0.5]}px`,
          padding: `${canvasSpacing[3]}px ${canvasSpacing[2]}px`,
        },
        style,
      )}
    >
      <div
        style={{
          fontSize: "24px",
          lineHeight: "28px",
          fontWeight: 600,
          fontVariantNumeric: "tabular-nums",
          color: tone ? statToneColors[tone] : tokens.text.primary,
        }}
      >
        {value}
      </div>
      <div
        style={{
          fontSize: canvasTypography.small.fontSize,
          lineHeight: canvasTypography.small.lineHeight,
          color: tokens.text.tertiary,
        }}
      >
        {label}
      </div>
    </div>
  );
}

export type CalloutTone = "info" | "success" | "warning" | "danger" | "neutral";
export type CalloutProps = {
  children?: ReactNode;
  tone?: CalloutTone;
  title?: ReactNode;
  icon?: ReactNode;
  style?: CSSProperties;
};

const calloutToneColors: Record<CalloutTone, string> = {
  info: chartPalette.lightBlue,
  success: chartPalette.lightGreen,
  warning: chartPalette.brightOrange,
  danger: chartPalette.darkAmber,
  neutral: chartPalette.muted,
};

export function Callout({
  children,
  tone = "info",
  title,
  icon,
  style,
}: CalloutProps): JSX.Element {
  const { tokens } = useHostTheme();
  const toneColor = calloutToneColors[tone];
  const lineHeight = canvasTypography.body.lineHeight;
  const iconBox = canvasSpacing[3];
  const root: CSSProperties = {
    boxSizing: "border-box",
    display: "flex",
    alignItems: "flex-start",
    gap: `${canvasSpacing[2.5]}px`,
    width: "100%",
    color: tokens.text.primary,
    fontSize: canvasTypography.body.fontSize,
    fontWeight: canvasTypography.body.fontWeight,
    lineHeight,
  };
  const iconWrap: CSSProperties = {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    flexShrink: 0,
    height: lineHeight,
    color: toneColor,
  };
  const leading =
    icon != null ? (
      <span style={{ ...iconWrap, minWidth: iconBox }}>{icon}</span>
    ) : (
      <span style={{ ...iconWrap, width: iconBox }} aria-hidden>
        <CalloutToneIcon tone={tone} color={toneColor} />
      </span>
    );
  return (
    <div role="note" style={mergeStyle(root, style)}>
      {leading}
      <div
        style={{
          flex: 1,
          minWidth: 0,
          display: "flex",
          flexDirection: "column",
          gap: `${canvasSpacing[1]}px`,
        }}
      >
        {title != null ? (
          <div
            style={{
              color: tokens.text.primary,
              fontWeight: 500,
              fontSize: canvasTypography.body.fontSize,
              lineHeight,
            }}
          >
            {title}
          </div>
        ) : null}
        {children != null ? (
          <div
            style={{
              color: tokens.text.primary,
              fontSize: canvasTypography.body.fontSize,
              fontWeight: canvasTypography.body.fontWeight,
              lineHeight,
            }}
          >
            {children}
          </div>
        ) : null}
      </div>
    </div>
  );
}
