// Vendored from peri-canvas@0.1.0 (packages/canvas/src/diff-view.tsx).
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// See PROVENANCE.md. Internal use only; do not redistribute without legal review.
import type { CSSProperties, JSX } from "react";
import { useHostTheme } from "./hooks";
import { mergeStyle } from "./merge-style";
import { canvasSpacing, canvasTypography, CANVAS_MONO_FONT_STACK } from "./theme";
import {
  inferLanguageFromPath,
  normalizeLanguage,
  tokenizeLine,
  type HighlightColors,
} from "./highlight";

const MONO_FONT = CANVAS_MONO_FONT_STACK;
const DIFF_FONT_SIZE = "12px";
const DIFF_GUTTER_FONT_SIZE = "11px";
const DIFF_LINE_HEIGHT = "20px";

export type DiffStatsProps = {
  additions?: number;
  deletions?: number;
  style?: CSSProperties;
};

export function DiffStats({
  additions = 0,
  deletions = 0,
  style,
}: DiffStatsProps): JSX.Element | null {
  const { tokens } = useHostTheme();
  if (additions <= 0 && deletions <= 0) return null;
  return (
    <span
      style={mergeStyle(
        {
          display: "inline-flex",
          alignItems: "center",
          gap: `${canvasSpacing[0.5]}px`,
          flexShrink: 0,
          fontVariantNumeric: "tabular-nums",
          fontSize: canvasTypography.small.fontSize,
          lineHeight: canvasTypography.small.lineHeight,
        },
        style,
      )}
    >
      {additions > 0 ? <span style={{ color: tokens.category.green }}>+{additions}</span> : null}
      {deletions > 0 ? <span style={{ color: tokens.category.red }}>-{deletions}</span> : null}
    </span>
  );
}

export type DiffLineType = "added" | "removed" | "unchanged";

export type DiffLineData = {
  type: DiffLineType;
  content: string;
  lineNumber?: number;
};

export type DiffViewProps = {
  lines: DiffLineData[];
  path?: string;
  language?: string;
  showLineNumbers?: boolean;
  coloredLineNumbers?: boolean;
  showAccentStrip?: boolean;
  style?: CSSProperties;
};

function getFontStyle(fontStyle?: number): CSSProperties {
  if (fontStyle === 2) return { fontStyle: "italic" };
  if (fontStyle === 1) return { fontWeight: 600 };
  return {};
}

function DiffViewLine({
  line,
  language,
  showLineNumbers,
  coloredLineNumbers,
  showAccentStrip,
  colors,
}: {
  line: DiffLineData;
  language?: string;
  showLineNumbers: boolean;
  coloredLineNumbers: boolean;
  showAccentStrip: boolean;
  colors: HighlightColors;
}) {
  const { tokens } = useHostTheme();
  const backgrounds = {
    added: tokens.diff.insertedLine,
    removed: tokens.diff.removedLine,
    unchanged: "transparent",
  };
  const strips = {
    added: tokens.diff.stripAdded,
    removed: tokens.diff.stripRemoved,
    unchanged: undefined as string | undefined,
  };
  const numberColors = {
    added: tokens.category.green,
    removed: tokens.category.red,
    unchanged: tokens.text.tertiary,
  };
  const tokensForLine =
    language !== undefined && line.content.length > 0
      ? tokenizeLine(line.content, language, colors)
      : null;
  return (
    <div
      style={{
        display: "flex",
        minWidth: "100%",
        minHeight: DIFF_LINE_HEIGHT,
        background: backgrounds[line.type],
      }}
    >
      {showAccentStrip ? (
        <div
          style={{
            width: "3px",
            flexShrink: 0,
            background: strips[line.type] ?? "transparent",
          }}
        />
      ) : null}
      {showLineNumbers ? (
        <div
          style={{
            display: "flex",
            flexShrink: 0,
            userSelect: "none",
            paddingRight: "4px",
          }}
        >
          <span
            style={{
              boxSizing: "content-box",
              minWidth: "4ch",
              textAlign: "right",
              paddingLeft: "4px",
              paddingRight: "4px",
              color: coloredLineNumbers ? numberColors[line.type] : tokens.text.tertiary,
              fontVariantNumeric: "tabular-nums",
              fontSize: DIFF_GUTTER_FONT_SIZE,
            }}
          >
            {line.lineNumber != null ? line.lineNumber : ""}
          </span>
        </div>
      ) : null}
      <div
        style={{
          flex: 1,
          paddingLeft: "8px",
          whiteSpace: "pre",
          overflow: "visible",
          color: tokens.text.primary,
        }}
      >
        {tokensForLine
          ? tokensForLine.map((token) => (
              <span
                key={token.col}
                style={{ color: token.color, ...getFontStyle(token.fontStyle) }}
              >
                {token.content}
              </span>
            ))
          : line.content}
      </div>
    </div>
  );
}

export function DiffView({
  lines = [],
  path,
  language,
  showLineNumbers = true,
  coloredLineNumbers = true,
  showAccentStrip = true,
  style,
}: DiffViewProps): JSX.Element {
  const { tokens } = useHostTheme();
  const resolved = normalizeLanguage(language) ?? (path ? inferLanguageFromPath(path) : undefined);
  const colors: HighlightColors = {
    keyword: tokens.category.purple,
    string: tokens.category.green,
    comment: tokens.text.quaternary,
    number: tokens.category.orange,
    type: tokens.category.cyan,
    punct: tokens.text.tertiary,
    text: tokens.text.primary,
  };
  return (
    <div
      style={mergeStyle(
        {
          boxSizing: "border-box",
          fontFamily: MONO_FONT,
          fontSize: DIFF_FONT_SIZE,
          lineHeight: DIFF_LINE_HEIGHT,
          background: tokens.bg.editor,
          overflow: "auto",
          tabSize: 4,
        },
        style,
      )}
    >
      <div style={{ minWidth: "100%", width: "max-content", paddingBlock: "2px" }}>
        {lines.map((line, index) => (
          <DiffViewLine
            key={index}
            line={line}
            language={resolved}
            showLineNumbers={showLineNumbers}
            coloredLineNumbers={coloredLineNumbers}
            showAccentStrip={showAccentStrip}
            colors={colors}
          />
        ))}
      </div>
    </div>
  );
}
