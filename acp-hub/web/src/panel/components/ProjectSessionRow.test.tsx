import { fireEvent, render, screen } from '@solidjs/testing-library';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { ProjectSessionInfo } from '../lib/registry-view';
import { ProjectSessionRow, type ProjectSessionRowProps } from './ProjectSessionRow';

const session: ProjectSessionInfo = {
  id: 'hub-abcdef12',
  projectId: 'p1',
  acpSessionId: 'acp-12345678',
  title: '架构重构',
  lifecycle: 'ready',
  updatedAt: '2026-08-13T10:00:00Z',
  lastOpenedAt: null,
  activeChatId: null,
  archivedAt: null,
};

function props(overrides: Partial<ProjectSessionRowProps> = {}): ProjectSessionRowProps {
  return {
    session,
    state: { label: '未启动 · 会话已保存', tone: 'idle' },
    selected: false,
    opening: false,
    navigationBusy: false,
    readOnly: false,
    renameOpen: false,
    menuOpen: false,
    runtimeActive: false,
    replacementBusy: false,
    onNavigate: vi.fn(),
    onOpen: vi.fn(),
    onSelectRuntime: vi.fn(),
    onRenameOpenChange: vi.fn(),
    onMenuOpenChange: vi.fn(),
    onRename: vi.fn(() => true),
    onCreateReplacement: vi.fn(),
    onArchiveRequest: vi.fn(),
    ...overrides,
  };
}

describe('ProjectSessionRow', () => {
  beforeEach(() => vi.clearAllMocks());

  it('delegates server-authoritative opening without navigating early', () => {
    const value = props();
    render(() => <ProjectSessionRow {...value} />);

    fireEvent.click(screen.getByRole('button', { name: /^架构重构/ }));

    expect(value.onOpen).toHaveBeenCalledWith('hub-abcdef12', value.onNavigate);
    expect(value.onNavigate).not.toHaveBeenCalled();
  });

  it('lets read-only users select an already running chat locally', () => {
    const value = props({
      readOnly: true,
      session: { ...session, activeChatId: 'chat-live' },
    });
    render(() => <ProjectSessionRow {...value} />);

    fireEvent.click(screen.getByRole('button', { name: /^架构重构/ }));

    expect(value.onSelectRuntime).toHaveBeenCalledWith('hub-abcdef12', 'chat-live');
    expect(value.onNavigate).toHaveBeenCalledOnce();
    expect(value.onOpen).not.toHaveBeenCalled();
  });

  it('submits a trimmed rename and closes only after committed', async () => {
    let commit: (() => void) | undefined;
    const value = props({
      renameOpen: true,
      onRename: vi.fn((_id, _name, onCommitted) => {
        commit = onCommitted;
        return true;
      }),
    });
    render(() => <ProjectSessionRow {...value} />);

    fireEvent.input(screen.getByRole('textbox', { name: '会话名称' }), { target: { value: '  新名称  ' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    expect(value.onRename).toHaveBeenCalledWith('hub-abcdef12', '新名称', expect.any(Function), expect.any(Function));
    expect(value.onRenameOpenChange).not.toHaveBeenCalledWith(false);
    commit?.();
    expect(value.onRenameOpenChange).toHaveBeenCalledWith(false);
  });

  it('exposes archive through a semantic menu and delegates confirmation ownership', () => {
    const value = props({ menuOpen: true });
    render(() => <ProjectSessionRow {...value} />);

    fireEvent.click(screen.getByRole('menuitem', { name: '归档会话' }));

    expect(value.onMenuOpenChange).toHaveBeenCalledWith(false);
    expect(value.onArchiveRequest).toHaveBeenCalledWith('hub-abcdef12');
  });

  it('does not allow a live runtime to be hidden from the sidebar', () => {
    const value = props({
      menuOpen: true,
      runtimeActive: true,
      session: { ...session, activeChatId: 'chat-live' },
    });
    render(() => <ProjectSessionRow {...value} />);

    expect(screen.getByRole('menuitem', { name: '归档会话' })).toBeDisabled();
    expect(screen.getByRole('menuitem', { name: '归档会话' })).toHaveAttribute('title', '请先关闭此会话的运行实例');
  });
});
