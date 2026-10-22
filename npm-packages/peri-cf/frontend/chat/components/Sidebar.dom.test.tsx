import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { cleanup, fireEvent, render, screen } = await import('@testing-library/react');
const { Sidebar } = await import('./Sidebar');
const { createElement } = await import('react');

afterEach(() => cleanup());

const chat = (id: string, title: string) => ({
  id, title, updatedAt: '2026-10-20T00:00:00Z',
  messageCount: 1, running: false, pendingInteractionCount: 0,
  lastTurnStatus: null, lastCompletedAt: null,
});

function setup(overrides: Record<string, unknown> = {}) {
  const selected: string[] = [];
  const refreshed: number[] = [];
  render(createElement(Sidebar, {
    chats: [chat('a', '会话 A'), chat('b', '会话 B')],
    selectedId: 'a', selectedRunning: false, loading: false, disabled: false, open: true,
    onClose: () => {}, onNew: () => {}, onRefresh: () => refreshed.push(1),
    onSelect: (target: { id: string }) => selected.push(target.id),
    ...overrides,
  }));
  return { selected, refreshed };
}

describe('Sidebar real click interactions', () => {
  test('clicking another chat selects it and clicking the selected row repeats the no-op selection', () => {
    const { selected } = setup();
    fireEvent.click(screen.getByText('会话 B'));
    fireEvent.click(screen.getByText('会话 A'));
    expect(selected).toEqual(['b', 'a']);
  });

  test('refresh and disabled navigation use real buttons', () => {
    const { refreshed } = setup();
    fireEvent.click(screen.getByLabelText('刷新会话列表'));
    expect(refreshed).toHaveLength(1);

    cleanup();
    const blocked = setup({ disabled: true });
    fireEvent.click(screen.getByText('会话 B'));
    expect(blocked.selected).toEqual([]);
  });

  test('search filters rows without changing selection identity', () => {
    const { selected } = setup();
    fireEvent.change(screen.getByLabelText('搜索会话'), { target: { value: 'B' } });
    expect(screen.queryByText('会话 A')).toBeNull();
    fireEvent.click(screen.getByText('会话 B'));
    expect(selected).toEqual(['b']);
  });

  test('a narrow closed drawer is inert and an open drawer is a focus-trapped dialog', () => {
    const original = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      matches: query === '(max-width: 767px)',
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => true,
    })) as typeof window.matchMedia;
    try {
      const closed = setup({ open: false });
      const closedAside = document.getElementById('peri-sidebar')!;
      expect(closedAside.getAttribute('role')).toBe('dialog');
      expect(closedAside.getAttribute('aria-hidden')).toBe('true');
      expect(closedAside.hasAttribute('inert')).toBe(true);
      expect(closedAside.contains(document.activeElement)).toBe(false);

      cleanup();
      setup({ open: true });
      const openAside = document.getElementById('peri-sidebar')!;
      expect(openAside.getAttribute('aria-modal')).toBe('true');
      expect(openAside.hasAttribute('inert')).toBe(false);
      // 焦点进入 drawer 的第一个可聚焦元素。
      expect(document.activeElement?.getAttribute('aria-label')).toBe('关闭会话列表');

      fireEvent.keyDown(openAside, { key: 'Tab', shiftKey: true });
      expect(openAside.contains(document.activeElement)).toBe(true);
    } finally {
      window.matchMedia = original;
    }
  });
});
