// Vendored from peri-canvas@0.1.0 (packages/canvas/src/merge-style.ts).
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// Internal use only; do not redistribute without legal review.
import type { CSSProperties } from "react";

export function mergeStyle(
  base: CSSProperties,
  override?: CSSProperties,
): CSSProperties {
  return override ? { ...base, ...override } : base;
}
