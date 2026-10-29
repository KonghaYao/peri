// Vendored from peri-canvas@0.1.0 (packages/canvas/src/chart-primitives.tsx).
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// Internal use only; do not redistribute without legal review.
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type JSX,
  type MouseEvent,
  type ReactNode,
} from "react";
import { useHostTheme } from "./hooks";
import { mergeStyle } from "./merge-style";
import type { CanvasTokens } from "./tokens";
import { chartColorSequence, chartPalette } from "./tokens";
import { canvasRadius, canvasSpacing, canvasTypography } from "./theme";

export type ChartTone = "success" | "danger" | "warning" | "info" | "neutral";

export type ChartDataPoint = {
  label: string;
  value: number;
};

export type ChartSeries = {
  name: string;
  data: number[];
  tone?: ChartTone;
};

export type ChartReferenceLine = {
  value: number;
  label?: string;
  tone?: ChartTone;
};

type ValueAxisProps = {
  beginAtZero?: boolean;
  yMin?: number;
  yMax?: number;
  referenceLines?: ChartReferenceLine[];
};

export type BarChartProps = ValueAxisProps & {
  categories: string[];
  series: ChartSeries[];
  height?: number;
  stacked?: boolean;
  horizontal?: boolean;
  normalized?: boolean;
  valueSuffix?: string;
  valuePrefix?: string;
  showValues?: boolean;
  style?: CSSProperties;
};

export type LineChartProps = ValueAxisProps & {
  categories: string[];
  series: ChartSeries[];
  height?: number;
  fill?: boolean;
  valueSuffix?: string;
  valuePrefix?: string;
  showValues?: boolean;
  showHoverGuide?: boolean;
  style?: CSSProperties;
};

export type PieChartProps = {
  data: Array<ChartDataPoint & { tone?: ChartTone }>;
  size?: number;
  donut?: boolean;
  style?: CSSProperties;
};

const FALLBACK_CHART_WIDTH = 480;
const DEFAULT_CHART_HEIGHT = 240;
const DEFAULT_PIE_SIZE = 200;
const VERTICAL_MARGINS = { top: 16, right: 12, bottom: 40, left: 48 };
const HORIZONTAL_BAR_MARGINS = { top: 12, right: 28, bottom: 24, left: 96 };

type Margins = { top: number; right: number; bottom: number; left: number };
type Scale = { ticks: number[]; min: number; max: number };
type TipEntry = { color: string; name: string; value: number };
type TipState = {
  x: number;
  y: number;
  label: string;
  entries: TipEntry[];
  total?: number;
};

function useMeasuredWidth() {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(FALLBACK_CHART_WIDTH);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const apply = (next?: number) => {
      const value = Math.round(next ?? el.getBoundingClientRect().width ?? el.clientWidth);
      if (value > 0) setWidth(value);
    };
    apply();
    // happy-dom / 旧浏览器没有 ResizeObserver：退化为初始化时测一次宽度。
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      apply(entries[0]?.contentRect.width);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return { ref, width };
}

function sanitize(value: number): number {
  return Number.isFinite(value) && value >= 0 ? value : 0;
}

function sanitizeFinite(value: number): number {
  return Number.isFinite(value) ? value : 0;
}

function trimZeros(value: string): string {
  return value.replace(/\.0+$/, "").replace(/(\.\d*[1-9])0+$/, "$1");
}

function formatValue(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1e6) return `${trimZeros((value / 1e6).toFixed(abs >= 1e7 ? 0 : 1))}M`;
  if (abs >= 1e3) return `${trimZeros((value / 1e3).toFixed(abs >= 1e4 ? 0 : 1))}K`;
  if (Number.isInteger(value)) return `${value}`;
  return trimZeros(value.toFixed(abs >= 10 ? 1 : 2));
}

function formatPercent(value: number): string {
  return value >= 10 ? `${Math.round(value)}%` : `${trimZeros(value.toFixed(1))}%`;
}

function formatWithAffix(value: number, prefix = "", suffix = ""): string {
  return `${prefix}${formatValue(value)}${suffix}`;
}

function truncateLabel(label: string, max = 12): string {
  return label.length <= max ? label : `${label.slice(0, Math.max(1, max - 3))}…`;
}

function estLabelWidthPx(labels: string[], maxChars: number): number {
  let longest = 1;
  for (const label of labels) longest = Math.max(longest, Math.min(label.length, maxChars));
  return 7 * longest + 14;
}

function fitStride(count: number, available: number, minSize: number): number {
  const maxLabels = Math.max(1, Math.floor(available / Math.max(minSize, 1)));
  return count <= maxLabels ? 1 : Math.ceil(count / maxLabels);
}

function niceStep(raw: number): number {
  if (!Number.isFinite(raw) || raw <= 0) return 1;
  const exp = 10 ** Math.floor(Math.log10(raw));
  const n = raw / exp;
  if (n <= 1) return exp;
  if (n <= 2) return 2 * exp;
  if (n <= 5) return 5 * exp;
  return 10 * exp;
}

function buildScale(
  minValue: number,
  maxValue: number,
  options: {
    beginAtZero?: boolean;
    yMin?: number;
    yMax?: number;
    extra?: number[];
  } = {},
  tickCount = 4,
): Scale {
  const { beginAtZero = true, yMin, yMax, extra = [] } = options;
  let lo = Number.isFinite(minValue) ? minValue : 0;
  let hi = Number.isFinite(maxValue) ? maxValue : 1;
  for (const value of extra) {
    if (Number.isFinite(value)) {
      lo = Math.min(lo, value);
      hi = Math.max(hi, value);
    }
  }
  let min = yMin ?? (beginAtZero ? Math.min(0, lo) : lo);
  let max = yMax ?? hi;
  if (!Number.isFinite(min)) min = 0;
  if (!Number.isFinite(max) || max <= min) max = min + 1;
  const step = niceStep((max - min) / Math.max(tickCount - 1, 1));
  if (yMin == null) min = Math.floor(min / step) * step;
  if (yMax == null) max = Math.ceil(max / step) * step;
  if (max <= min) max = min + step;
  const ticks: number[] = [];
  for (let tick = min; tick <= max + step / 2; tick += step) {
    ticks.push(Number(tick.toFixed(10)));
  }
  const last = ticks[ticks.length - 1];
  if (last == null || last < max - 1e-6 * step) ticks.push(Number(max.toFixed(10)));
  return { ticks, min, max };
}

function valueFrac(scale: Scale, value: number): number {
  const span = scale.max - scale.min;
  if (span <= 0) return 0;
  const frac = (sanitizeFinite(value) - scale.min) / span;
  if (frac < 0) return 0;
  if (frac > 1) return 1;
  return frac;
}

function toneToColor(tone?: ChartTone): string | undefined {
  if (tone == null) return undefined;
  switch (tone) {
    case "success":
      return chartPalette.lightGreen;
    case "danger":
      return chartPalette.darkAmber;
    case "warning":
      return chartPalette.brightOrange;
    case "info":
      return chartPalette.lightBlue;
    case "neutral":
      return chartPalette.muted;
  }
}

function seriesColor(index: number, tone?: ChartTone): string {
  return toneToColor(tone) ?? chartColorSequence[index % chartColorSequence.length];
}

function barColor(series: ChartSeries[], seriesIndex: number, categoryIndex: number): string {
  const fromTone = toneToColor(series[seriesIndex].tone);
  if (fromTone != null) return fromTone;
  return series.length === 1
    ? chartColorSequence[categoryIndex % chartColorSequence.length]
    : chartColorSequence[seriesIndex % chartColorSequence.length];
}

function svgTextProps(theme: CanvasTokens) {
  return {
    fill: theme.text.secondary,
    fontFamily: "inherit",
    fontSize: canvasTypography.small.fontSize,
  };
}

function renderYAxis(
  width: number,
  height: number,
  margins: Margins,
  scale: Scale,
  theme: CanvasTokens,
  suffix = "",
  prefix = "",
): ReactNode {
  const plotHeight = Math.max(height - margins.top - margins.bottom, 1);
  const text = svgTextProps(theme);
  return scale.ticks.map((tick) => {
    const y = margins.top + plotHeight - valueFrac(scale, tick) * plotHeight;
    return (
      <g key={tick}>
        {tick > scale.min ? (
          <line
            x1={margins.left}
            x2={width - margins.right}
            y1={y}
            y2={y}
            stroke={theme.stroke.tertiary}
            strokeWidth={1}
          />
        ) : null}
        <text x={margins.left - canvasSpacing[2]} y={y + 4} textAnchor="end" {...text}>
          {formatWithAffix(tick, prefix, suffix)}
        </text>
      </g>
    );
  });
}

function refLabelChip(
  label: string,
  x: number,
  y: number,
  anchor: "end" | "middle",
  color: string,
  theme: CanvasTokens,
) {
  const width = 6.5 * label.length + 8;
  const left = anchor === "end" ? x - width : x - width / 2;
  return (
    <g>
      <rect x={left} y={y - 11} width={width} height={14} fill={theme.bg.editor} rx={2} />
      <text
        x={x}
        y={y}
        textAnchor={anchor}
        fill={color}
        fontFamily="inherit"
        fontSize={canvasTypography.small.fontSize}
        fontWeight={500}
      >
        {label}
      </text>
    </g>
  );
}

function renderReferenceLines(
  orientation: "horizontal" | "vertical",
  width: number,
  height: number,
  margins: Margins,
  scale: Scale,
  lines: ChartReferenceLine[] | undefined,
  theme: CanvasTokens,
): ReactNode {
  if (!lines || lines.length === 0) return null;
  const plotWidth = Math.max(width - margins.left - margins.right, 1);
  const plotHeight = Math.max(height - margins.top - margins.bottom, 1);
  return lines.map((line) => {
    if (!Number.isFinite(line.value)) return null;
    if (line.value < scale.min || line.value > scale.max) return null;
    const color = toneToColor(line.tone) ?? theme.text.tertiary;
    const key = `ref-${line.value}-${line.label ?? ""}`;
    if (orientation === "horizontal") {
      const y = margins.top + plotHeight - valueFrac(scale, line.value) * plotHeight;
      return (
        <g key={key}>
          <line
            x1={margins.left}
            x2={width - margins.right}
            y1={y}
            y2={y}
            stroke={color}
            strokeWidth={1}
            strokeDasharray="4 3"
          />
          {line.label ? refLabelChip(line.label, width - margins.right, y - 3, "end", color, theme) : null}
        </g>
      );
    }
    const x = margins.left + valueFrac(scale, line.value) * plotWidth;
    return (
      <g key={key}>
        <line
          x1={x}
          x2={x}
          y1={margins.top}
          y2={height - margins.bottom}
          stroke={color}
          strokeWidth={1}
          strokeDasharray="4 3"
        />
        {line.label ? refLabelChip(line.label, x, margins.top - 2, "middle", color, theme) : null}
      </g>
    );
  });
}

function emptyState(height: number, theme: CanvasTokens, style?: CSSProperties) {
  return (
    <div
      style={mergeStyle(
        {
          width: "100%",
          minWidth: 0,
          height: `${height}px`,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          borderRadius: `${canvasRadius.md}px`,
          border: `1px solid ${theme.stroke.secondary}`,
          background: theme.fill.quaternary,
          color: theme.text.secondary,
          fontSize: canvasTypography.small.fontSize,
          lineHeight: canvasTypography.small.lineHeight,
        },
        style,
      )}
    >
      No data
    </div>
  );
}

function useTooltip() {
  const [tip, setTip] = useState<TipState | null>(null);
  return {
    tip,
    show: useCallback(
      (x: number, y: number, label: string, entries: TipEntry[], total?: number) => {
        setTip({ x, y, label, entries, total });
      },
      [],
    ),
    hide: useCallback(() => setTip(null), []),
  };
}

function TooltipOverlay({
  tip,
  theme,
  containerWidth,
  suffix,
  prefix,
}: {
  tip: TipState;
  theme: CanvasTokens;
  containerWidth: number;
  suffix?: string;
  prefix?: string;
}) {
  const left = tip.x + 180 + 16 > containerWidth ? Math.max(0, tip.x - 180 - 12) : tip.x + 12;
  return (
    <div
      style={{
        position: "absolute",
        top: `${Math.max(tip.y - 8, 0)}px`,
        left: `${left}px`,
        minWidth: "180px",
        maxWidth: "260px",
        pointerEvents: "none",
        zIndex: 10,
        padding: `${canvasSpacing[1.5]}px ${canvasSpacing[2]}px`,
        background: theme.bg.chrome,
        border: `1px solid ${theme.stroke.primary}`,
        borderRadius: `${canvasRadius.lg}px`,
        fontSize: canvasTypography.small.fontSize,
        lineHeight: canvasTypography.small.lineHeight,
        color: theme.text.primary,
      }}
    >
      <div style={{ fontWeight: 600, marginBottom: `${canvasSpacing[0.5]}px` }}>{tip.label}</div>
      {tip.entries.map((entry) => (
        <div
          key={entry.name}
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: `${canvasSpacing[3]}px`,
            marginBottom: "1px",
          }}
        >
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: `${canvasSpacing[1]}px`,
              color: theme.text.secondary,
            }}
          >
            <span
              style={{
                display: "inline-block",
                width: "8px",
                height: "8px",
                borderRadius: `${canvasRadius.full}px`,
                background: entry.color,
                flexShrink: 0,
              }}
            />
            <span>{entry.name}</span>
          </div>
          <span
            style={{
              fontWeight: 600,
              fontVariantNumeric: "tabular-nums",
              color: theme.text.primary,
            }}
          >
            {formatWithAffix(entry.value, prefix ?? "", suffix ?? "")}
          </span>
        </div>
      ))}
      {tip.total != null ? (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: `${canvasSpacing[3]}px`,
            marginTop: `${canvasSpacing[0.5]}px`,
            paddingTop: `${canvasSpacing[0.5]}px`,
            borderTop: `1px solid ${theme.stroke.secondary}`,
          }}
        >
          <span style={{ color: theme.text.secondary }}>Total</span>
          <span
            style={{
              fontWeight: 600,
              fontVariantNumeric: "tabular-nums",
              color: theme.text.primary,
            }}
          >
            {formatWithAffix(tip.total, prefix ?? "", suffix ?? "")}
          </span>
        </div>
      ) : null}
    </div>
  );
}

function Legend({
  items,
  theme,
}: {
  items: Array<{ name: string; color: string }>;
  theme: CanvasTokens;
}) {
  if (items.length <= 1) return null;
  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        justifyContent: "center",
        gap: `${canvasSpacing[2]}px ${canvasSpacing[3]}px`,
        marginTop: `${canvasSpacing[2]}px`,
        fontSize: canvasTypography.small.fontSize,
        lineHeight: canvasTypography.small.lineHeight,
        color: theme.text.secondary,
      }}
    >
      {items.map((item) => (
        <div
          key={item.name}
          style={{ display: "inline-flex", alignItems: "center", gap: `${canvasSpacing[1]}px` }}
        >
          <span
            style={{
              width: "8px",
              height: "8px",
              borderRadius: `${canvasRadius.full}px`,
              background: item.color,
              flexShrink: 0,
            }}
          />
          <span>{item.name}</span>
        </div>
      ))}
    </div>
  );
}

export function BarChart({
  categories = [],
  series: rawSeries = [],
  height = DEFAULT_CHART_HEIGHT,
  stacked = false,
  horizontal = false,
  normalized = false,
  valueSuffix = "",
  valuePrefix = "",
  showValues,
  beginAtZero = true,
  yMin,
  yMax,
  referenceLines,
  style,
}: BarChartProps): JSX.Element {
  const { tokens } = useHostTheme();
  const { ref, width } = useMeasuredWidth();
  const { tip, show, hide } = useTooltip();
  const series = rawSeries.map((item) => ({ ...item, data: item.data ?? [] }));
  const isStacked = stacked || normalized;
  const categoryCount = categories.length;
  const seriesCount = series.length;
  if (categoryCount === 0 || seriesCount === 0) return emptyState(height, tokens, style);
  const printValues = !isStacked && (showValues ?? (seriesCount === 1 && categoryCount <= 8));
  const svgWidth = Math.max(width, 1);
  const margins = horizontal
    ? { ...HORIZONTAL_BAR_MARGINS, right: printValues ? 48 : HORIZONTAL_BAR_MARGINS.right }
    : VERTICAL_MARGINS;
  const plotWidth = Math.max(svgWidth - margins.left - margins.right, 1);
  const plotHeight = Math.max(height - margins.top - margins.bottom, 1);
  const stride = horizontal
    ? fitStride(categoryCount, plotHeight, 18)
    : fitStride(categoryCount, plotWidth, estLabelWidthPx(categories, 12));
  const stackTotals: number[] = [];
  if (isStacked) {
    for (let i = 0; i < categoryCount; i++) {
      let sum = 0;
      for (const item of series) sum += sanitize(item.data[i] ?? 0);
      stackTotals.push(sum);
    }
  }
  const extras = (referenceLines ?? []).map((line) => line.value).filter((value) => Number.isFinite(value));
  let scale: Scale;
  if (normalized) {
    scale = buildScale(0, 100, { yMin: 0, yMax: 100, extra: extras });
  } else if (isStacked) {
    scale = buildScale(0, Math.max(...stackTotals, 0), { beginAtZero: true, extra: extras });
  } else {
    let lo = Number.POSITIVE_INFINITY;
    let hi = 0;
    for (const item of series) {
      for (const value of item.data) {
        const n = sanitize(value);
        lo = Math.min(lo, n);
        hi = Math.max(hi, n);
      }
    }
    if (!Number.isFinite(lo)) lo = 0;
    scale = buildScale(lo, hi, { beginAtZero, yMin, yMax, extra: extras });
  }
  const suffix = normalized ? "%" : valueSuffix;
  const prefix = normalized ? "" : valuePrefix;
  const onHover = (index: number, x: number, y: number) => {
    const entries = series.map((item, seriesIndex) => {
      const raw = sanitize(item.data[index] ?? 0);
      const value = normalized && stackTotals[index] > 0 ? (raw / stackTotals[index]) * 100 : raw;
      return { color: barColor(series, seriesIndex, index), name: item.name, value };
    });
    const total = isStacked && !normalized ? stackTotals[index] : undefined;
    show(x, y, categories[index], entries, total);
  };
  const text = svgTextProps(tokens);

  if (horizontal) {
    const band = plotHeight / Math.max(categoryCount, 1);
    return (
      <div
        ref={ref}
        style={mergeStyle({ width: "100%", minWidth: 0, position: "relative" }, style)}
        onMouseLeave={hide}
      >
        <svg
          width={svgWidth}
          height={height}
          viewBox={`0 0 ${svgWidth} ${height}`}
          role="img"
          aria-label="Bar chart"
          style={{ display: "block", width: "100%", height: `${height}px` }}
        >
          {scale.ticks.map((tick) => {
            const x = margins.left + valueFrac(scale, tick) * plotWidth;
            return (
              <g key={tick}>
                {tick > scale.min ? (
                  <line
                    x1={x}
                    x2={x}
                    y1={margins.top}
                    y2={height - margins.bottom}
                    stroke={tokens.stroke.tertiary}
                    strokeWidth={1}
                  />
                ) : null}
                <text x={x} y={height - canvasSpacing[1]} textAnchor="middle" {...text}>
                  {formatWithAffix(tick, prefix, suffix)}
                </text>
              </g>
            );
          })}
          {categories.map((category, index) => {
            const y = margins.top + index * band;
            const showLabel = (categoryCount - 1 - index) % stride === 0;
            const label = showLabel ? (
              <text
                x={margins.left - canvasSpacing[2]}
                y={y + band / 2}
                textAnchor="end"
                dominantBaseline="middle"
                {...text}
              >
                {truncateLabel(category, 14)}
              </text>
            ) : null;
            const onMove = (event: MouseEvent<SVGGElement>) => {
              const box = event.currentTarget.ownerSVGElement!.getBoundingClientRect();
              onHover(index, event.clientX - box.left, event.clientY - box.top);
            };
            if (isStacked) {
              const total = stackTotals[index];
              let cursor = 0;
              return (
                <g key={category} onMouseMove={onMove}>
                  {label}
                  {series.map((item, seriesIndex) => {
                    const raw = sanitize(item.data[index] ?? 0);
                    const widthPx =
                      (total > 0 ? (normalized ? raw / total : raw / scale.max) : 0) * plotWidth;
                    const x = margins.left + cursor;
                    cursor += widthPx;
                    return (
                      <rect
                        key={item.name}
                        x={x}
                        y={y + 0.15 * band}
                        width={Math.max(widthPx, 0)}
                        height={0.7 * band}
                        fill={barColor(series, seriesIndex, index)}
                        rx={index === 0 || index === categoryCount - 1 ? 2 : 0}
                      />
                    );
                  })}
                </g>
              );
            }
            const barH = (0.7 * band) / seriesCount;
            return (
              <g key={category} onMouseMove={onMove}>
                {label}
                {series.map((item, seriesIndex) => {
                  const raw = sanitize(item.data[index] ?? 0);
                  const widthPx = valueFrac(scale, raw) * plotWidth;
                  const barY = y + 0.15 * band + seriesIndex * barH;
                  return (
                    <g key={item.name}>
                      <rect
                        x={margins.left}
                        y={barY}
                        width={widthPx}
                        height={Math.max(barH - 1, 1)}
                        fill={barColor(series, seriesIndex, index)}
                      />
                      {printValues && raw > 0 ? (
                        <text
                          x={margins.left + widthPx + canvasSpacing[1.5]}
                          y={barY + barH / 2}
                          textAnchor="start"
                          dominantBaseline="middle"
                          fill={tokens.text.primary}
                          fontFamily="inherit"
                          fontSize={canvasTypography.small.fontSize}
                          fontWeight={500}
                        >
                          {formatWithAffix(raw, prefix, suffix)}
                        </text>
                      ) : null}
                    </g>
                  );
                })}
              </g>
            );
          })}
          {renderReferenceLines("vertical", svgWidth, height, margins, scale, referenceLines, tokens)}
        </svg>
        {tip ? (
          <TooltipOverlay
            tip={tip}
            theme={tokens}
            containerWidth={svgWidth}
            suffix={suffix}
            prefix={prefix}
          />
        ) : null}
        <Legend
          items={series.map((item, index) => ({
            name: item.name,
            color: barColor(series, index, 0),
          }))}
          theme={tokens}
        />
      </div>
    );
  }

  const band = plotWidth / Math.max(categoryCount, 1);
  return (
    <div
      ref={ref}
      style={mergeStyle({ width: "100%", minWidth: 0, position: "relative" }, style)}
      onMouseLeave={hide}
    >
      <svg
        width={svgWidth}
        height={height}
        viewBox={`0 0 ${svgWidth} ${height}`}
        role="img"
        aria-label="Bar chart"
        style={{ display: "block", width: "100%", height: `${height}px` }}
      >
        {renderYAxis(svgWidth, height, margins, scale, tokens, suffix, prefix)}
        {categories.map((category, index) => {
          const x = margins.left + index * band;
          const showLabel = (categoryCount - 1 - index) % stride === 0;
          const onMove = (event: MouseEvent<SVGGElement>) => {
            const box = event.currentTarget.ownerSVGElement!.getBoundingClientRect();
            onHover(index, event.clientX - box.left, event.clientY - box.top);
          };
          if (isStacked) {
            const total = stackTotals[index];
            let cursor = 0;
            const barW = Math.min(0.7 * band, 56);
            const barX = x + (band - barW) / 2;
            return (
              <g key={category} onMouseMove={onMove}>
                <rect x={x} y={margins.top} width={band} height={plotHeight} fill="transparent" />
                {[...series].reverse().map((item, reverseIndex) => {
                  const seriesIndex = seriesCount - 1 - reverseIndex;
                  const raw = sanitize(item.data[index] ?? 0);
                  const h = (total > 0 ? (normalized ? raw / total : raw / scale.max) : 0) * plotHeight;
                  const y = margins.top + plotHeight - cursor - h;
                  cursor += h;
                  return (
                    <rect
                      key={item.name}
                      x={barX}
                      y={y}
                      width={barW}
                      height={Math.max(h, 0)}
                      fill={barColor(series, seriesIndex, index)}
                    />
                  );
                })}
                {showLabel ? (
                  <text x={x + band / 2} y={height - canvasSpacing[1]} textAnchor="middle" {...text}>
                    {truncateLabel(category)}
                  </text>
                ) : null}
              </g>
            );
          }
          const groupW = 0.7 * band;
          const barW = groupW / seriesCount;
          const groupX = x + (band - groupW) / 2;
          return (
            <g key={category} onMouseMove={onMove}>
              <rect x={x} y={margins.top} width={band} height={plotHeight} fill="transparent" />
              {series.map((item, seriesIndex) => {
                const raw = sanitize(item.data[index] ?? 0);
                const h = valueFrac(scale, raw) * plotHeight;
                const barX = groupX + seriesIndex * barW;
                const y = margins.top + plotHeight - h;
                const showLabelOnBar = seriesCount === 1 || barW >= 14;
                return (
                  <g key={item.name}>
                    <rect
                      x={barX}
                      y={y}
                      width={Math.max(barW - 1, 1)}
                      height={h}
                      fill={barColor(series, seriesIndex, index)}
                    />
                    {printValues && showLabelOnBar && raw > 0 ? (
                      <text
                        x={barX + barW / 2}
                        y={Math.max(y - 4, margins.top)}
                        textAnchor="middle"
                        fill={tokens.text.primary}
                        fontFamily="inherit"
                        fontSize={canvasTypography.small.fontSize}
                        fontWeight={500}
                      >
                        {formatWithAffix(raw, prefix, suffix)}
                      </text>
                    ) : null}
                  </g>
                );
              })}
              {showLabel ? (
                <text x={x + band / 2} y={height - canvasSpacing[1]} textAnchor="middle" {...text}>
                  {truncateLabel(category)}
                </text>
              ) : null}
            </g>
          );
        })}
        {renderReferenceLines("horizontal", svgWidth, height, margins, scale, referenceLines, tokens)}
      </svg>
      {tip ? (
        <TooltipOverlay
          tip={tip}
          theme={tokens}
          containerWidth={svgWidth}
          suffix={suffix}
          prefix={prefix}
        />
      ) : null}
      <Legend
        items={series.map((item, index) => ({
          name: item.name,
          color: barColor(series, index, 0),
        }))}
        theme={tokens}
      />
    </div>
  );
}

export function LineChart({
  categories = [],
  series: rawSeries = [],
  height = DEFAULT_CHART_HEIGHT,
  fill = false,
  valueSuffix = "",
  valuePrefix = "",
  showValues = false,
  showHoverGuide = true,
  beginAtZero = true,
  yMin,
  yMax,
  referenceLines,
  style,
}: LineChartProps): JSX.Element {
  const { tokens } = useHostTheme();
  const { ref, width } = useMeasuredWidth();
  const series = rawSeries.map((item) => ({ ...item, data: item.data ?? [] }));
  const { tip, show, hide } = useTooltip();
  const categoryCount = categories.length;
  const seriesCount = series.length;
  if (categoryCount === 0 || seriesCount === 0) return emptyState(height, tokens, style);
  const svgWidth = Math.max(width, 1);
  const margins = VERTICAL_MARGINS;
  const plotWidth = Math.max(svgWidth - margins.left - margins.right, 1);
  const plotHeight = Math.max(height - margins.top - margins.bottom, 1);
  const stride = fitStride(categoryCount, plotWidth, estLabelWidthPx(categories, 12));
  const extras = (referenceLines ?? []).map((line) => line.value).filter((value) => Number.isFinite(value));
  let lo = Number.POSITIVE_INFINITY;
  let hi = 0;
  for (const item of series) {
    for (const value of item.data) {
      const n = sanitize(value);
      lo = Math.min(lo, n);
      hi = Math.max(hi, n);
    }
  }
  if (!Number.isFinite(lo)) lo = 0;
  const scale = buildScale(lo, hi, { beginAtZero, yMin, yMax, extra: extras });
  const colors = series.map((item, index) => seriesColor(index, item.tone));
  const text = svgTextProps(tokens);
  const guideX =
    showHoverGuide && tip
      ? Math.max(margins.left, Math.min(margins.left + plotWidth, tip.x))
      : null;
  const xAt = (index: number) =>
    categoryCount === 1 ? margins.left + plotWidth / 2 : margins.left + (plotWidth * index) / (categoryCount - 1);
  const yAt = (value: number) => margins.top + plotHeight - valueFrac(scale, value) * plotHeight;

  return (
    <div
      ref={ref}
      style={mergeStyle({ width: "100%", minWidth: 0, position: "relative" }, style)}
      onMouseLeave={hide}
    >
      <svg
        width={svgWidth}
        height={height}
        viewBox={`0 0 ${svgWidth} ${height}`}
        role="img"
        aria-label="Line chart"
        style={{ display: "block", width: "100%", height: `${height}px` }}
      >
        {renderYAxis(svgWidth, height, margins, scale, tokens, valueSuffix, valuePrefix)}
        {series.map((item, seriesIndex) => {
          const color = colors[seriesIndex];
          const points = item.data.map((value, index) => ({ x: xAt(index), y: yAt(value) }));
          if (points.length === 0) return null;
          const line = points
            .map((point, index) => `${index === 0 ? "M" : "L"} ${point.x} ${point.y}`)
            .join(" ");
          const area =
            fill && points.length >= 2
              ? [
                  `M ${points[0].x} ${margins.top + plotHeight}`,
                  ...points.map((point) => `L ${point.x} ${point.y}`),
                  `L ${points[points.length - 1].x} ${margins.top + plotHeight}`,
                  "Z",
                ].join(" ")
              : undefined;
          return (
            <g key={item.name}>
              {area ? <path d={area} fill={color} fillOpacity={0.12} stroke="none" /> : null}
              <path
                d={line}
                fill="none"
                stroke={color}
                strokeWidth={2}
                strokeLinecap="round"
                strokeLinejoin="round"
              />
              {categoryCount <= 20
                ? points.map((point, index) => (
                    <circle
                      key={`${item.name}-${categories[index] ?? index}`}
                      cx={point.x}
                      cy={point.y}
                      r={2.5}
                      fill={color}
                    />
                  ))
                : null}
              {showValues && categoryCount <= 20
                ? points.map((point, index) => (
                    <text
                      key={`label-${item.name}-${categories[index] ?? index}`}
                      x={point.x}
                      y={Math.max(point.y - 7, margins.top + 2)}
                      textAnchor="middle"
                      fill={tokens.text.primary}
                      fontFamily="inherit"
                      fontSize={canvasTypography.small.fontSize}
                      fontWeight={500}
                    >
                      {formatWithAffix(sanitize(item.data[index] ?? 0), valuePrefix, valueSuffix)}
                    </text>
                  ))
                : null}
            </g>
          );
        })}
        {categories.map((category, index) => {
          const x = categoryCount === 1 ? margins.left : xAt(index);
          const band = categoryCount === 1 ? plotWidth : plotWidth / (categoryCount - 1);
          return (
            <g key={category}>
              <rect
                x={x - band / 2}
                y={margins.top}
                width={band}
                height={plotHeight}
                fill="transparent"
                onMouseMove={(event) => {
                  const box = event.currentTarget.ownerSVGElement!.getBoundingClientRect();
                  const entries = series.map((item, seriesIndex) => ({
                    color: colors[seriesIndex],
                    name: item.name,
                    value: sanitize(item.data[index] ?? 0),
                  }));
                  show(event.clientX - box.left, event.clientY - box.top, categories[index], entries);
                }}
              />
              {(categoryCount - 1 - index) % stride === 0 ? (
                <text x={x} y={height - canvasSpacing[1]} textAnchor="middle" {...text}>
                  {truncateLabel(category)}
                </text>
              ) : null}
            </g>
          );
        })}
        {renderReferenceLines("horizontal", svgWidth, height, margins, scale, referenceLines, tokens)}
        {guideX != null ? (
          <line
            data-hover-guide="true"
            x1={guideX}
            x2={guideX}
            y1={margins.top}
            y2={margins.top + plotHeight}
            stroke={tokens.stroke.focused}
            strokeDasharray="4 4"
            strokeLinecap="round"
            strokeWidth={1.5}
            opacity={0.72}
            pointerEvents="none"
            vectorEffect="non-scaling-stroke"
          />
        ) : null}
      </svg>
      {tip ? (
        <TooltipOverlay
          tip={tip}
          theme={tokens}
          containerWidth={svgWidth}
          suffix={valueSuffix}
          prefix={valuePrefix}
        />
      ) : null}
      <Legend
        items={series.map((item, index) => ({ name: item.name, color: colors[index] }))}
        theme={tokens}
      />
    </div>
  );
}

function polarToCartesian(cx: number, cy: number, radius: number, angle: number) {
  const rad = ((angle - 90) * Math.PI) / 180;
  return { x: cx + radius * Math.cos(rad), y: cy + radius * Math.sin(rad) };
}

function arcPath(cx: number, cy: number, radius: number, start: number, end: number) {
  const dest = polarToCartesian(cx, cy, radius, end);
  return `A ${radius} ${radius} 0 ${end - start > 180 ? 1 : 0} 1 ${dest.x} ${dest.y}`;
}

function slicePath(
  donut: boolean,
  cx: number,
  cy: number,
  outer: number,
  inner: number,
  start: number,
  end: number,
) {
  if (!donut) {
    const startPt = polarToCartesian(cx, cy, outer, start);
    return [`M ${cx} ${cy}`, `L ${startPt.x} ${startPt.y}`, arcPath(cx, cy, outer, start, end), "Z"].join(
      " ",
    );
  }
  const outerStart = polarToCartesian(cx, cy, outer, start);
  const innerEnd = polarToCartesian(cx, cy, inner, end);
  const innerStart = polarToCartesian(cx, cy, inner, start);
  const large = end - start > 180 ? 1 : 0;
  return [
    `M ${outerStart.x} ${outerStart.y}`,
    arcPath(cx, cy, outer, start, end),
    `L ${innerEnd.x} ${innerEnd.y}`,
    `A ${inner} ${inner} 0 ${large} 0 ${innerStart.x} ${innerStart.y}`,
    "Z",
  ].join(" ");
}

export function PieChart({
  data = [],
  size = DEFAULT_PIE_SIZE,
  donut = false,
  style,
}: PieChartProps): JSX.Element {
  const { tokens } = useHostTheme();
  const [hover, setHover] = useState<number | null>(null);
  const slices = data.map((point, index) => ({
    ...point,
    value: sanitize(point.value),
    color: toneToColor(point.tone) ?? chartColorSequence[index % chartColorSequence.length],
  }));
  const total = slices.reduce((sum, slice) => sum + slice.value, 0);
  if (slices.length === 0 || total <= 0) return emptyState(size, tokens, style);
  const cx = size / 2;
  const cy = size / 2;
  const radius = Math.max(size / 2 - canvasSpacing[2], 16);
  const inner = donut ? 0.55 * radius : 0;
  const hoverRadius = radius + 4;
  const positive = slices.filter((slice) => slice.value > 0);
  const single = positive.length === 1;
  const singleIndex = single ? slices.findIndex((slice) => slice.value > 0) : null;
  const paths: Array<{
    d: string;
    dHover: string;
    color: string;
    label: string;
    value: number;
    dataIndex: number;
  }> = [];
  if (!single) {
    let angle = 0;
    for (let i = 0; i < slices.length; i++) {
      const slice = slices[i];
      const next = angle + (slice.value / total) * 360;
      if (slice.value > 0) {
        paths.push({
          d: slicePath(donut, cx, cy, radius, inner, angle, next),
          dHover: slicePath(donut, cx, cy, hoverRadius, inner, angle, next),
          color: slice.color,
          label: slice.label,
          value: slice.value,
          dataIndex: i,
        });
      }
      angle = next;
    }
  }
  const active = hover !== null ? slices[hover] : undefined;
  return (
    <div
      style={mergeStyle(
        {
          width: "100%",
          minWidth: 0,
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          gap: `${canvasSpacing[3]}px`,
          position: "relative",
        },
        style,
      )}
      onMouseLeave={() => setHover(null)}
    >
      <svg
        width={size + 8}
        height={size + 8}
        viewBox={`-4 -4 ${size + 8} ${size + 8}`}
        role="img"
        aria-label="Pie chart"
        style={{ display: "block" }}
      >
        {single ? (
          <g onMouseEnter={() => setHover(singleIndex)} onMouseLeave={() => setHover(null)}>
            <circle
              cx={cx}
              cy={cy}
              r={hover === singleIndex ? hoverRadius : radius}
              fill={positive[0].color}
              style={{ transition: "r 120ms ease" }}
            />
            {donut ? <circle cx={cx} cy={cy} r={inner} fill={tokens.bg.editor} /> : null}
          </g>
        ) : (
          paths.map((path) => (
            <path
              key={path.label}
              d={hover === path.dataIndex ? path.dHover : path.d}
              fill={path.color}
              fillOpacity={hover === null || hover === path.dataIndex ? 1 : 0.4}
              style={{ transition: "d 120ms ease, fill-opacity 120ms ease" }}
              onMouseEnter={() => setHover(path.dataIndex)}
              onMouseLeave={() => setHover(null)}
            />
          ))
        )}
        {donut ? (
          <g style={{ pointerEvents: "none" }}>
            <text
              x={cx}
              y={cy - 5}
              textAnchor="middle"
              dominantBaseline="middle"
              fill={tokens.text.primary}
              fontFamily="inherit"
              fontSize={canvasTypography.body.fontSize}
              fontWeight={600}
            >
              {formatValue(total)}
            </text>
            <text
              x={cx}
              y={cy + 12}
              textAnchor="middle"
              dominantBaseline="middle"
              fill={tokens.text.tertiary}
              fontFamily="inherit"
              fontSize={canvasTypography.small.fontSize}
            >
              Total
            </text>
          </g>
        ) : null}
      </svg>
      {active && active.value > 0 ? (
        <div
          style={{
            position: "absolute",
            top: `${size + 12}px`,
            left: "50%",
            transform: "translateX(-50%)",
            pointerEvents: "none",
            zIndex: 10,
            padding: `${canvasSpacing[1.5]}px ${canvasSpacing[2]}px`,
            background: tokens.bg.chrome,
            border: `1px solid ${tokens.stroke.primary}`,
            borderRadius: `${canvasRadius.lg}px`,
            fontSize: canvasTypography.small.fontSize,
            lineHeight: canvasTypography.small.lineHeight,
            color: tokens.text.primary,
            whiteSpace: "nowrap",
          }}
        >
          <span style={{ fontWeight: 600 }}>{active.label}</span>
          {" · "}
          {formatValue(active.value)} ({formatPercent((active.value / total) * 100)})
        </div>
      ) : null}
      <Legend
        items={slices.map((slice) => ({ name: slice.label, color: slice.color }))}
        theme={tokens}
      />
    </div>
  );
}
