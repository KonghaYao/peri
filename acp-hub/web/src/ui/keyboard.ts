export function primaryShortcut(key: string): string {
  if (typeof navigator === 'undefined') return `Ctrl+${key.toUpperCase()}`;
  const platform = navigator.platform || navigator.userAgent;
  return /mac|iphone|ipad|ipod/i.test(platform) ? `⌘${key.toUpperCase()}` : `Ctrl+${key.toUpperCase()}`;
}
