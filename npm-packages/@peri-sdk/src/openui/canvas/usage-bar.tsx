import type { CSSProperties, JSX, ReactNode } from "react";
import type { Color } from "./tokens";
import { usageColorSequence } from "./tokens";
import { useHostTheme } from "./hooks";
import { mergeStyle } from "./merge-style";
import { canvasSpacing, canvasTypography } from "./theme";

export interface UsageBarSegment {
  readonly id: string;
  readonly value: number;
  readonly color?: Color;
}

export type UsageBarProps = {
  readonly segments: readonly UsageBarSegment[];
  readonly total: number;
  readonly topLeftLabel?: ReactNode;
  readonly topRightLabel?: ReactNode;
  readonly style?: CSSProperties;
};

function safeValue(value: number): number {
  return Number.isFinite(value) && value > 0 ? value : 0;
}

export function UsageBar({
  segments,
  total,
  topLeftLabel,
  topRightLabel,
  style,
}: UsageBarProps): JSX.Element {
  const { tokens } = useHostTheme();
  const safeTotal = safeValue(total);
  const used = segments.reduce((sum, segment) => sum + safeValue(segment.value), 0);
  const remainder = Math.max(0, safeTotal - used);
  const showLabels = topLeftLabel != null || topRightLabel != null;
  const pill: CSSProperties = {
    height: 5,
    flexBasis: 0,
    flexShrink: 0,
    minWidth: canvasSpacing[1],
    borderRadius: 1,
  };
  return (
    <div
      style={mergeStyle(
        {
          display: "flex",
          flexDirection: "column",
          gap: `${canvasSpacing[2]}px`,
          width: "100%",
          minWidth: 0,
        },
        style,
      )}
    >
      {showLabels ? (
        <div
          style={{
            display: "flex",
            alignItems: "baseline",
            justifyContent: "space-between",
            gap: `${canvasSpacing[3]}px`,
            minWidth: 0,
            fontSize: canvasTypography.small.fontSize,
            lineHeight: canvasTypography.small.lineHeight,
            fontWeight: canvasTypography.small.fontWeight,
            color: tokens.text.secondary,
            fontVariantNumeric: "tabular-nums",
          }}
        >
          <span style={{ minWidth: 0 }}>{topLeftLabel}</span>
          <span style={{ minWidth: 0, textAlign: "right" }}>{topRightLabel}</span>
        </div>
      ) : null}
      <div
        role="img"
        aria-label={`Usage bar: ${used} of ${safeTotal} used`}
        style={{
          width: "100%",
          minWidth: 0,
          overflow: "hidden",
          borderRadius: 1,
          background: "transparent",
        }}
      >
        <div style={{ display: "flex", minWidth: 0, gap: 1 }}>
          {segments.map((segment, index) => {
            const color =
              segment.color ?? usageColorSequence[index % usageColorSequence.length];
            return (
              <span
                key={segment.id}
                aria-hidden="true"
                style={{
                  ...pill,
                  flexGrow: safeValue(segment.value),
                  background: tokens.category[color],
                }}
              />
            );
          })}
          <span
            aria-hidden="true"
            style={{ ...pill, flexGrow: remainder, background: tokens.fill.tertiary }}
          />
        </div>
      </div>
    </div>
  );
}
