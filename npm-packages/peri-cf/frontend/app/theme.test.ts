import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { installDom } from '../test/dom';
import { syncThemeClass, watchSystemTheme } from './theme';

installDom();

const rootClasses = (): DOMTokenList => document.documentElement.classList;

function stubMatchMedia(matches: boolean): {
  readonly listeners: Set<(event: MediaQueryListEvent) => void>;
  readonly queries: string[];
  readonly restore: () => void;
} {
  const original = window.matchMedia;
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  const queries: string[] = [];
  window.matchMedia = ((query: string) => {
    queries.push(query);
    return {
      matches,
      media: query,
      onchange: null,
      addEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => {
        listeners.add(listener);
      },
      removeEventListener: (_type: string, listener: (event: MediaQueryListEvent) => void) => {
        listeners.delete(listener);
      },
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    } as unknown as MediaQueryList;
  }) as typeof window.matchMedia;
  return { listeners, queries, restore: () => { window.matchMedia = original; } };
}

describe('theme class sync', () => {
  test('dark preference adds .dark and removes .light', () => {
    syncThemeClass(true);
    expect(rootClasses().contains('dark')).toBe(true);
    expect(rootClasses().contains('light')).toBe(false);
  });

  test('light preference adds .light and removes .dark', () => {
    syncThemeClass(false);
    expect(rootClasses().contains('dark')).toBe(false);
    expect(rootClasses().contains('light')).toBe(true);
  });

  test('repeated sync is idempotent', () => {
    syncThemeClass(true);
    syncThemeClass(true);
    expect(rootClasses().contains('dark')).toBe(true);
    expect(rootClasses().contains('light')).toBe(false);
  });
});

describe('system theme watching', () => {
  test('registers a prefers-color-scheme listener and syncs on change', () => {
    const media = stubMatchMedia(false);
    try {
      const stop = watchSystemTheme();
      expect(media.queries).toEqual(['(prefers-color-scheme: dark)']);
      expect(media.listeners.size).toBe(1);

      const [listener] = [...media.listeners];
      listener!({ matches: true } as MediaQueryListEvent);
      expect(rootClasses().contains('dark')).toBe(true);
      expect(rootClasses().contains('light')).toBe(false);

      stop();
      expect(media.listeners.size).toBe(0);
    } finally {
      media.restore();
    }
  });

  test('the cleanup function removes exactly the registered listener', () => {
    const media = stubMatchMedia(true);
    try {
      const stop = watchSystemTheme();
      const [listener] = [...media.listeners];
      expect(listener).toBeDefined();
      stop();
      expect(media.listeners.has(listener!)).toBe(false);
    } finally {
      media.restore();
    }
  });

  test('an environment without matchMedia subscribes to nothing', () => {
    const original = window.matchMedia;
    // happy-dom 始终提供 matchMedia；删除模拟受限环境。
    delete (window as { matchMedia?: typeof window.matchMedia }).matchMedia;
    try {
      const stop = watchSystemTheme();
      expect(typeof stop).toBe('function');
      expect(() => stop()).not.toThrow();
    } finally {
      window.matchMedia = original;
    }
  });
});

describe('chat token theme contract', () => {
  test('dark chat tokens follow the same .dark class as Tailwind', () => {
    const css = readFileSync(new URL('../styles/chat-tokens.css', import.meta.url), 'utf8');
    // 修 P0 前：深色在 `@media (prefers-color-scheme: dark)` 内，Tailwind 的 `.dark`
    // 从未被设置，两套 token 会在系统深色下错位。
    expect(css).not.toContain('prefers-color-scheme');
    expect(css).toContain(':root.dark {');
  });
});
