/**
 * 主题 class 同步：把系统深浅色偏好写到 `<html>` 的 `.dark` / `.light`。
 *
 * Tailwind（`frontend/tailwind.css`）与 chat token（`frontend/styles/chat-tokens.css`）
 * 共用同一个 `.dark` class 源；`.light` 是显式亮色标记，与 `.dark` 互斥。
 * 首帧由 `index.html` 的内联脚本先行设置，这里负责运行时跟随系统变化。
 */
const DARK_SCHEME_QUERY = '(prefers-color-scheme: dark)';

/** 同步 `<html>` 上的主题 class；重复调用是幂等的。 */
export function syncThemeClass(matches: boolean): void {
  const root = document.documentElement;
  root.classList.toggle('dark', matches);
  root.classList.toggle('light', !matches);
}

/** 监听系统主题变化；返回清理函数，调用后不再写 class。无 matchMedia 时为空订阅。 */
export function watchSystemTheme(): () => void {
  if (typeof window.matchMedia !== 'function') return () => {};
  const query = window.matchMedia(DARK_SCHEME_QUERY);
  const onChange = (event: MediaQueryListEvent): void => syncThemeClass(event.matches);
  query.addEventListener('change', onChange);
  return () => query.removeEventListener('change', onChange);
}
