// New in peri-openui (no vendored upstream). Shape inspired by t3code's
// ContextWindowMeter; no t3code code or runtime dependency is copied.
import type { CSSProperties, JSX } from "react";
import { useHostTheme } from "./hooks";
import { mergeStyle } from "./merge-style";
import { canvasSpacing, canvasTypography } from "./theme";

export type GaugeTone = "success" | "danger" | "warning" | "info";

export type GaugeProps = {
  value: number;
  label: string;
  max?: number;
  unit?: string;
  detail?: string;
  tone?: GaugeTone;
  size?: number;
  style?: CSSProperties;
};

const DEFAULT_MAX = 100;
const DEFAULT_SIZE = 96;
const MIN_SIZE = 72;
const MAX_SIZE = 160;
const SVG_RADIUS = 42;
const SVG_STROKE_WIDTH = 8;
const SVG_CIRCUMFERENCE = 2 * Math.PI * SVG_RADIUS;

const toneColorKeys: Record<GaugeTone, "green" | "red" | "yellow" | "blue"> = {
  success: "green",
  danger: "red",
  warning: "yellow",
  info: "blue",
};

function resolveSize(value: number | undefined): number {
  if (value === undefined || !Number.isFinite(value)) return DEFAULT_SIZE;
  return Math.min(MAX_SIZE, Math.max(MIN_SIZE, Math.round(value)));
}

/** 保留最多两位小数，避免浮点噪声把环形中心撑爆。 */
export function formatGaugeValue(value: number): string {
  if (Number.isInteger(value)) return String(value);
  return String(Number(value.toFixed(2)));
}

export function Gauge({
  value,
  label,
  max = DEFAULT_MAX,
  unit = "",
  detail,
  tone,
  size,
  style,
}: GaugeProps): JSX.Element {
  const { tokens } = useHostTheme();
  const dimension = resolveSize(size);
  const invalid =
    !Number.isFinite(value) || !Number.isFinite(max) || max <= 0;

  if (invalid) {
    const reason = !Number.isFinite(value)
      ? `value ${String(value)} must be a finite number`
      : !Number.isFinite(max)
        ? `max ${String(max)} must be a finite number`
        : `max ${String(max)} must be greater than 0`;
    return (
      <div
        role="status"
        data-gauge-state="invalid"
        style={mergeStyle(
          {
            display: "inline-flex",
            flexDirection: "column",
            gap: `${canvasSpacing[1]}px`,
            boxSizing: "border-box",
            maxWidth: "100%",
            padding: `${canvasSpacing[2]}px ${canvasSpacing[3]}px`,
            border: `1px solid ${tokens.stroke.secondary}`,
            borderRadius: 6,
            background: tokens.fill.quaternary,
            color: tokens.text.secondary,
            fontSize: canvasTypography.small.fontSize,
            lineHeight: canvasTypography.small.lineHeight,
          },
          style,
        )}
      >
        <span style={{ color: tokens.category.yellow, fontWeight: 600 }}>Gauge unavailable</span>
        <span style={{ overflowWrap: "anywhere" }}>
          {label ? `${label} · ` : ""}
          {reason}
        </span>
      </div>
    );
  }

  const safeValue = Math.min(max, Math.max(0, value));
  const ratio = safeValue / max;
  const dashOffset = SVG_CIRCUMFERENCE * (1 - ratio);
  const progressColor = tone ? tokens.category[toneColorKeys[tone]] : tokens.text.tertiary;
  const valueText = `${formatGaugeValue(value)}${unit} of ${formatGaugeValue(max)}${unit}`;

  return (
    <div
      role="meter"
      aria-label={label || "Gauge"}
      aria-valuemin={0}
      aria-valuemax={max}
      aria-valuenow={safeValue}
      aria-valuetext={valueText}
      data-gauge-state="ready"
      style={mergeStyle(
        {
          display: "inline-flex",
          flexDirection: "column",
          alignItems: "center",
          gap: `${canvasSpacing[1]}px`,
          maxWidth: "100%",
          minWidth: dimension,
        },
        style,
      )}
    >
      <div
        style={{
          position: "relative",
          width: dimension,
          height: dimension,
          flexShrink: 0,
        }}
      >
        <svg
          viewBox="0 0 100 100"
          width="100%"
          height="100%"
          aria-hidden="true"
          focusable="false"
          style={{ display: "block", transform: "rotate(-90deg)" }}
        >
          <circle
            cx="50"
            cy="50"
            r={SVG_RADIUS}
            fill="none"
            stroke={tokens.stroke.primary}
            strokeWidth={SVG_STROKE_WIDTH}
          />
          <circle
            cx="50"
            cy="50"
            r={SVG_RADIUS}
            fill="none"
            stroke={progressColor}
            strokeWidth={SVG_STROKE_WIDTH}
            strokeLinecap="round"
            strokeDasharray={SVG_CIRCUMFERENCE}
            strokeDashoffset={dashOffset}
          />
        </svg>
        <div
          style={{
            position: "absolute",
            inset: 0,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            padding: `0 ${Math.round(dimension * 0.18)}px`,
            boxSizing: "border-box",
            overflow: "hidden",
          }}
        >
          <span
            style={{
              display: "inline-flex",
              alignItems: "baseline",
              justifyContent: "center",
              gap: 1,
              maxWidth: "100%",
              color: tokens.text.primary,
              fontSize: Math.max(14, Math.round(dimension * 0.19)),
              lineHeight: 1.15,
              fontWeight: 600,
              fontVariantNumeric: "tabular-nums",
              whiteSpace: "nowrap",
            }}
          >
            <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
              {formatGaugeValue(value)}
            </span>
            {unit ? (
              <span
                style={{
                  fontSize: "0.62em",
                  fontWeight: 500,
                  color: tokens.text.secondary,
                }}
              >
                {unit}
              </span>
            ) : null}
          </span>
        </div>
      </div>
      <div
        style={{
          maxWidth: "100%",
          textAlign: "center",
          color: tokens.text.secondary,
          fontSize: canvasTypography.small.fontSize,
          lineHeight: canvasTypography.small.lineHeight,
          fontWeight: 500,
          overflowWrap: "anywhere",
        }}
      >
        {label}
      </div>
      {detail ? (
        <div
          style={{
            maxWidth: "100%",
            textAlign: "center",
            color: tokens.text.tertiary,
            fontSize: 11,
            lineHeight: "14px",
            overflowWrap: "anywhere",
          }}
        >
          {detail}
        </div>
      ) : null}
    </div>
  );
}
