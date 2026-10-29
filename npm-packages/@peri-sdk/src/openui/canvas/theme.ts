export type { CanvasPalette, CanvasTokens } from "./tokens";
export {
  canvasPaletteDark,
  canvasPaletteLight,
  canvasTokens,
  canvasTokensLight,
} from "./tokens";

export const canvasTypography = {
  // 标题字号对齐 peri-cf 的 chat-markdown.css；正文保持 14/20 以提高信息密度。
  h1: { fontSize: "21px", lineHeight: "30px", fontWeight: 590 },
  h2: { fontSize: "19px", lineHeight: "28px", fontWeight: 590 },
  h3: { fontSize: "18px", lineHeight: "26px", fontWeight: 590 },
  body: { fontSize: "14px", lineHeight: "20px", fontWeight: 400 },
  small: { fontSize: "12px", lineHeight: "16px", fontWeight: 400 },
} as const;

export const canvasSpacing = {
  0.5: 2,
  1: 4,
  1.5: 6,
  2: 8,
  2.5: 10,
  3: 12,
  3.5: 14,
  4: 16,
  4.5: 18,
  5: 20,
  6: 24,
  7: 28,
  8: 32,
  9: 36,
  10: 40,
} as const;

export type CanvasSpacing = typeof canvasSpacing;

export const canvasRadius = {
  none: 0,
  xs: 2,
  sm: 4,
  md: 6,
  lg: 8,
  xl: 12,
  full: 9999,
} as const;

export type CanvasRadius = typeof canvasRadius;

export const CANVAS_SANS_FONT_STACK =
  '-apple-system, BlinkMacSystemFont, "Segoe WPC", "Segoe UI", system-ui, "Ubuntu", "Droid Sans", sans-serif';

export const CANVAS_MONO_FONT_STACK =
  'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace';

export const CANVAS_BODY_PADDING_VERTICAL_PX = 24;
export const CANVAS_BODY_PADDING_HORIZONTAL_PX = 32;
