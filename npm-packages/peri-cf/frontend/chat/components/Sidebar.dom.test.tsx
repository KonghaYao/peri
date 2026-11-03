import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { act, cleanup, fireEvent, render, screen } = await import('@testing-library/react');
const { Sidebar } = await import('./Sidebar');
const { createElement } = await import('react');

afterEach(() => cleanup());

const chat = (sessionId: string, title: string) => ({
  sessionId, title, updatedAt: '2026-10-20T00:00:00Z',
  cwd: '/workspace', messageCount: 1, createdAt: '2026-10-20T00:00:00Z',
  projectId: 'default',
});

function setup(overrides: Record<string, unknown> = {}) {
  const selected: string[] = [];
  const refreshed: number[] = [];
  const projectNew: string[] = [];
  const created: string[] = [];
  render(createElement(Sidebar, {
    chats: [chat('a', '会话 A'), chat('b', '会话 B')],
    projects: [{ id: 'default', name: '默认项目', createdAt: '', isDefault: true }],
    selectedId: 'a', selectedRunning: false, loading: false, projectsLoading: false, disabled: false, open: true,
    onClose: () => {}, onNew: () => {}, onRefresh: () => refreshed.push(1),
    onNewInProject: (id: string) => projectNew.push(id),
    onCreateProject: async (name: string) => { created.push(name); },
    onSelect: (target: { sessionId: string }) => selected.push(target.sessionId),
    ...overrides,
  }));
  return { selected, refreshed, projectNew, created };
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

describe('Sidebar project grouping', () => {
  const project = (id: string, name: string, isDefault = false) => ({
    id, name, createdAt: '2026-10-20T00:00:00Z', isDefault,
  });

  test('groups chats under projects and reports per-project counts', () => {
    setup({
      chats: [
        { ...chat('a', '项目会话'), projectId: 'p1' },
        { ...chat('b', '默认会话'), projectId: 'default' },
      ],
      projects: [project('default', '默认项目', true), project('p1', '项目 A')],
    });
    expect(screen.getByLabelText('项目 A，1 个会话')).toBeTruthy();
    expect(screen.getByLabelText('默认项目，1 个会话')).toBeTruthy();
    expect(screen.getByText('项目会话')).toBeTruthy();
    expect(screen.getByText('默认会话')).toBeTruthy();
  });

  test('collapsing a project hides its chats until expanded again', () => {
    setup({
      chats: [{ ...chat('a', '项目会话'), projectId: 'p1' }],
      projects: [project('default', '默认项目', true), project('p1', '项目 A')],
    });
    fireEvent.click(screen.getByLabelText('项目 A，1 个会话'));
    expect(screen.queryByText('项目会话')).toBeNull();
    expect(screen.getByLabelText('项目 A，1 个会话').getAttribute('aria-expanded')).toBe('false');
    fireEvent.click(screen.getByLabelText('项目 A，1 个会话'));
    expect(screen.getByText('项目会话')).toBeTruthy();
  });

  test('new session carries the project id; default project has no inline entry', () => {
    const { projectNew } = setup({
      projects: [project('default', '默认项目', true), project('p1', '项目 A')],
    });
    expect(screen.queryByLabelText('在 默认项目 中新建会话')).toBeNull();
    fireEvent.click(screen.getByLabelText('在 项目 A 中新建会话'));
    expect(projectNew).toEqual(['p1']);
  });

  test('creates a project inline and keeps the form with an alert on failure', async () => {
    const { created } = setup();
    fireEvent.click(screen.getByLabelText('新建项目'));
    fireEvent.change(screen.getByLabelText('项目名称'), { target: { value: '新项目' } });
    await act(async () => {
      fireEvent.submit(screen.getByLabelText('新建项目表单'));
    });
    expect(created).toEqual(['新项目']);
    expect(screen.queryByLabelText('新建项目表单')).toBeNull();

    cleanup();
    setup({ onCreateProject: async () => { throw new Error('项目名称已存在：新项目'); } });
    fireEvent.click(screen.getByLabelText('新建项目'));
    fireEvent.change(screen.getByLabelText('项目名称'), { target: { value: '新项目' } });
    await act(async () => {
      fireEvent.submit(screen.getByLabelText('新建项目表单'));
    });
    expect(screen.getByRole('alert').textContent).toContain('项目名称已存在');
  });
});
