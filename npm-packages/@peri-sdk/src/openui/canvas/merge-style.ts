import type { CSSProperties } from "react";

export function mergeStyle(
  base: CSSProperties,
  override?: CSSProperties,
): CSSProperties {
  return override ? { ...base, ...override } : base;
}
