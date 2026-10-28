// Vendored from peri-canvas@0.1.0 (packages/canvas/src/tokens.ts).
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// See PROVENANCE.md. Internal use only; do not redistribute without legal review.
export interface CanvasPalette {
  readonly foreground: string;
  readonly foregroundSecondary: string;
  readonly foregroundTertiary: string;
  readonly foregroundQuaternary: string;
  readonly editor: string;
  readonly chrome: string;
  readonly sidebar: string;
  readonly elevated: string;
  readonly fillPrimary: string;
  readonly fillSecondary: string;
  readonly fillTertiary: string;
  readonly fillQuaternary: string;
  readonly strokePrimary: string;
  readonly strokeSecondary: string;
  readonly strokeTertiary: string;
  readonly strokeFocused: string;
  readonly accent: string;
  readonly buttonBackground: string;
  readonly buttonForeground: string;
  readonly buttonHoverBackground: string;
  readonly link: string;
  readonly diffInsertedLine: string;
  readonly diffRemovedLine: string;
  readonly diffStripAdded: string;
  readonly diffStripRemoved: string;
}

const ON_ACCENT_DARK = "#191C22";
const ON_ACCENT_LIGHT = "#FFFFFF";

export const canvasPaletteDark: CanvasPalette = {
  foreground: "#FFFFFF",
  foregroundSecondary: "#EBEBEB",
  foregroundTertiary: "#C7C7C7",
  foregroundQuaternary: "#A8A8A8",
  editor: "#1A1A1A",
  chrome: "#161616",
  sidebar: "#1A1A1A",
  elevated: "#1A1A1A",
  fillPrimary: "#FFFFFF2E",
  fillSecondary: "#FFFFFF1F",
  fillTertiary: "#FFFFFF14",
  fillQuaternary: "#FFFFFF0A",
  strokePrimary: "#FFFFFF2B",
  strokeSecondary: "#FFFFFF1C",
  strokeTertiary: "#FFFFFF12",
  strokeFocused: "#FFFFFF",
  accent: "#599CE7",
  buttonBackground: "#599CE7",
  buttonForeground: ON_ACCENT_DARK,
  buttonHoverBackground: "#68A4E8",
  link: "#7BAFE9",
  diffInsertedLine: "#3FA26633",
  diffRemovedLine: "#B8004933",
  diffStripAdded: "#3FA2668F",
  diffStripRemoved: "#FC6B838F",
};

export const canvasPaletteLight: CanvasPalette = {
  foreground: "#141414",
  foregroundSecondary: "#3A3A3A",
  // 浅色下提升小字对比度（约 4.6:1 on #f9fafb），见 PROVENANCE.md 修改清单。
  foregroundTertiary: "#6B6F76",
  foregroundQuaternary: "#8A8D93",
  editor: "#FFFFFF",
  chrome: "#FAFAFA",
  sidebar: "#F7F7F7",
  elevated: "#FFFFFF",
  fillPrimary: "#1414141F",
  fillSecondary: "#14141414",
  fillTertiary: "#1414140F",
  fillQuaternary: "#14141408",
  strokePrimary: "#1414141F",
  strokeSecondary: "#14141414",
  strokeTertiary: "#1414140F",
  strokeFocused: "#2778C1",
  accent: "#2778C1",
  buttonBackground: "#2778C1",
  buttonForeground: "#FCFCFC",
  buttonHoverBackground: "#256EB0",
  link: "#2778C1",
  diffInsertedLine: "#00AF6624",
  diffRemovedLine: "#FF617B38",
  diffStripAdded: "#007041CC",
  diffStripRemoved: "#BE1744CC",
};

export const chartPalette = {
  green: "#1F8A65E8",
  darkGreen: "#0D855AE0",
  lightGreen: "#52B896E0",
  mintGreen: "#7DCAB0E0",
  blue: "#2E79B5E0",
  lightBlue: "#70B0D8E0",
  indigo: "#5A6CC0F0",
  lightIndigo: "#9AAADCE0",
  purple: "#7B64B8F0",
  lightPurple: "#AA98D8E0",
  warmPink: "#C85898E0",
  lightPink: "#E8A0C4E0",
  brightOrange: "#F0A040E0",
  deepOrange: "#C06028E0",
  goldenYellow: "#E8C030E0",
  darkAmber: "#C04848E0",
  warmPeach: "#F0A088E0",
  vibrantTeal: "#2A9A8AE0",
  muted: "#8888A8E0",
  neutralLine: "#888899D0",
} as const;

export type Color =
  | "gray"
  | "purple"
  | "green"
  | "yellow"
  | "cyan"
  | "pink"
  | "blue"
  | "orange"
  | "red";

export type CategoryPalette = Readonly<Record<Color, string>>;

export const categoryPaletteDark: CategoryPalette = {
  gray: canvasPaletteDark.foregroundTertiary,
  purple: "#9386F2",
  green: "#3FA266",
  yellow: "#F1B467",
  cyan: "#81A1C1",
  pink: "#B48EAD",
  blue: "#7BAFE9",
  orange: "#DD7F76",
  red: "#FC6B83",
};

export const categoryPaletteLight: CategoryPalette = {
  gray: canvasPaletteLight.foregroundTertiary,
  purple: "#7565CC",
  green: "#007041",
  yellow: "#A46700",
  cyan: "#176C74",
  pink: "#92156A",
  blue: "#2778C1",
  orange: "#C93714",
  red: "#BE1744",
};

export const colorPalette: CategoryPalette = categoryPaletteDark;

export const usageColorSequence: readonly Color[] = [
  "gray",
  "purple",
  "green",
  "yellow",
  "cyan",
  "pink",
  "blue",
  "orange",
  "red",
];

export const chartColorSequence: readonly string[] = [
  chartPalette.green,
  chartPalette.lightBlue,
  chartPalette.indigo,
  chartPalette.brightOrange,
  chartPalette.deepOrange,
  chartPalette.goldenYellow,
  chartPalette.warmPink,
  chartPalette.warmPeach,
  chartPalette.purple,
  chartPalette.mintGreen,
  chartPalette.muted,
  chartPalette.vibrantTeal,
];

export interface CanvasTokens {
  bg: {
    editor: string;
    chrome: string;
    elevated: string;
  };
  text: {
    primary: string;
    secondary: string;
    tertiary: string;
    quaternary: string;
    link: string;
    onAccent: string;
  };
  stroke: {
    primary: string;
    secondary: string;
    tertiary: string;
    focused: string;
  };
  fill: {
    primary: string;
    secondary: string;
    tertiary: string;
    quaternary: string;
  };
  accent: {
    primary: string;
    control: string;
    controlHover: string;
  };
  diff: {
    insertedLine: string;
    removedLine: string;
    stripAdded: string;
    stripRemoved: string;
  };
  category: CategoryPalette;
}

export function buildTokens(
  palette: CanvasPalette,
  category: CategoryPalette,
): CanvasTokens {
  return {
    bg: {
      editor: palette.editor,
      chrome: palette.chrome,
      elevated: palette.elevated,
    },
    text: {
      primary: palette.foreground,
      secondary: palette.foregroundSecondary,
      tertiary: palette.foregroundTertiary,
      quaternary: palette.foregroundQuaternary,
      link: palette.link,
      onAccent: palette.buttonForeground,
    },
    stroke: {
      primary: palette.strokePrimary,
      secondary: palette.strokeSecondary,
      tertiary: palette.strokeTertiary,
      focused: palette.strokeFocused,
    },
    fill: {
      primary: palette.fillPrimary,
      secondary: palette.fillSecondary,
      tertiary: palette.fillTertiary,
      quaternary: palette.fillQuaternary,
    },
    accent: {
      primary: palette.accent,
      control: palette.buttonBackground,
      controlHover: palette.buttonHoverBackground,
    },
    diff: {
      insertedLine: palette.diffInsertedLine,
      removedLine: palette.diffRemovedLine,
      stripAdded: palette.diffStripAdded,
      stripRemoved: palette.diffStripRemoved,
    },
    category,
  };
}

export const canvasTokens = buildTokens(canvasPaletteDark, categoryPaletteDark);
export const canvasTokensLight = buildTokens(
  canvasPaletteLight,
  categoryPaletteLight,
);

export interface CanvasHostThemeOverrides {
  readonly primary?: string;
  readonly editorBackground?: string;
  readonly editorForeground?: string;
}

function parseHexColor(value: string): { r: number; g: number; b: number } | undefined {
  const hex = value.trim().replace(/^#/, "");
  if (!/^[0-9a-fA-F]+$/.test(hex)) return undefined;
  let r: string;
  let g: string;
  let b: string;
  if (hex.length === 3 || hex.length === 4) {
    r = hex[0] + hex[0];
    g = hex[1] + hex[1];
    b = hex[2] + hex[2];
  } else if (hex.length === 6 || hex.length === 8) {
    r = hex.slice(0, 2);
    g = hex.slice(2, 4);
    b = hex.slice(4, 6);
  } else {
    return undefined;
  }
  return {
    r: Number.parseInt(r, 16),
    g: Number.parseInt(g, 16),
    b: Number.parseInt(b, 16),
  };
}

export function applyWorkbenchSurfaces(
  palette: CanvasPalette,
  surfaces: Pick<CanvasHostThemeOverrides, "editorBackground" | "editorForeground">,
): CanvasPalette {
  let next = palette;
  if (
    surfaces.editorBackground &&
    parseHexColor(surfaces.editorBackground) !== undefined
  ) {
    next = {
      ...next,
      editor: surfaces.editorBackground,
      chrome: surfaces.editorBackground,
      elevated: surfaces.editorBackground,
    };
  }
  if (
    surfaces.editorForeground &&
    parseHexColor(surfaces.editorForeground) !== undefined
  ) {
    next = { ...next, foreground: surfaces.editorForeground };
  }
  return next;
}

const BT601_RED_WEIGHT = 299;
const BT601_GREEN_WEIGHT = 587;
const BT601_BLUE_WEIGHT = 114;
const ON_ACCENT_BRIGHTNESS_THRESHOLD = 150;

export function applyPrimaryColor(
  palette: CanvasPalette,
  primary: string,
): CanvasPalette {
  const rgb = parseHexColor(primary);
  if (!rgb) return palette;
  const brightness =
    (rgb.r * BT601_RED_WEIGHT +
      rgb.g * BT601_GREEN_WEIGHT +
      rgb.b * BT601_BLUE_WEIGHT) /
    1000;
  const onAccent =
    brightness > ON_ACCENT_BRIGHTNESS_THRESHOLD ? ON_ACCENT_DARK : ON_ACCENT_LIGHT;
  return {
    ...palette,
    accent: primary,
    buttonBackground: primary,
    buttonHoverBackground: primary,
    strokeFocused: primary,
    link: primary,
    buttonForeground: onAccent,
  };
}

export function isLightKind(kind: string): boolean {
  return kind === "light" || kind === "hc-light";
}

export function buildHostTokens(
  kind: string,
  overrides?: CanvasHostThemeOverrides,
): { tokens: CanvasTokens; palette: CanvasPalette } {
  const light = isLightKind(kind);
  let palette = applyWorkbenchSurfaces(
    light ? canvasPaletteLight : canvasPaletteDark,
    {
      editorBackground: overrides?.editorBackground,
      editorForeground: overrides?.editorForeground,
    },
  );
  if (overrides?.primary) {
    palette = applyPrimaryColor(palette, overrides.primary);
  }
  return {
    tokens: buildTokens(
      palette,
      light ? categoryPaletteLight : categoryPaletteDark,
    ),
    palette,
  };
}
