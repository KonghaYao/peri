import { setHostTheme } from "./canvas/hooks";

/**
 * peri-cf 聊天主题 -> canvas host tokens 的桥。
 *
 * 只读取三个 CSS 变量，避免把 Tailwind 颜色体系整体复制到 canvas：
 * - `--chat-code-bg` -> editorBackground（卡片/代码表面）
 * - `--chat-fg` -> editorForeground
 * - `--chat-link` -> primary/accent
 *
 * 变量值必须是 hex。`buildHostTokens` 只解析 hex；未来若改成 `oklch()`，
 * 它会静默忽略并回退 canvas 自带 palette（需要在文档中注明）。
 */
const LIGHT_FALLBACKS = {
  editorBackground: "#f9fafb",
  editorForeground: "#0f1115",
  primary: "#4176e6",
} as const;

const DARK_FALLBACKS = {
  editorBackground: "#1b1b1c",
  editorForeground: "#f9fafb",
  primary: "#7aaaff",
} as const;

/** 与 `chat-tokens.css` 的判定顺序一致：显式 class 优先，其次系统偏好。 */
export function resolveOpenUIThemeKind(
  root: Pick<HTMLElement, "classList">,
  prefersDark: boolean,
): "light" | "dark" {
  if (root.classList.contains("light")) return "light";
  if (root.classList.contains("dark")) return "dark";
  return prefersDark ? "dark" : "light";
}

function readVariable(style: CSSStyleDeclaration, name: string, fallback: string): string {
  return style.getPropertyValue(name).trim() || fallback;
}

function applyTheme(): void {
  const root = document.documentElement;
  const prefersDark =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches;
  const kind = resolveOpenUIThemeKind(root, prefersDark);
  const fallbacks = kind === "dark" ? DARK_FALLBACKS : LIGHT_FALLBACKS;
  const style = getComputedStyle(root);
  setHostTheme({
    kind,
    editorBackground: readVariable(style, "--chat-code-bg", fallbacks.editorBackground),
    editorForeground: readVariable(style, "--chat-fg", fallbacks.editorForeground),
    primary: readVariable(style, "--chat-link", fallbacks.primary),
  });
}

/**
 * 在 CSS import 之后调用一次。返回卸载函数，供测试/未来热切换使用。
 * 生产环境当前没有运行时主题切换；监听是为未来兼容保留的最小成本。
 */
export function installOpenUITheme(): () => void {
  if (typeof document === "undefined") return () => {};

  applyTheme();

  const media =
    typeof window.matchMedia === "function"
      ? window.matchMedia("(prefers-color-scheme: dark)")
      : undefined;
  const onMediaChange = () => applyTheme();
  media?.addEventListener("change", onMediaChange);

  const observer = new MutationObserver(() => applyTheme());
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["class"],
  });

  return () => {
    media?.removeEventListener("change", onMediaChange);
    observer.disconnect();
  };
}
