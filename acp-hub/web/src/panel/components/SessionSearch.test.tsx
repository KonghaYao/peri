import { fireEvent, render, screen } from '@solidjs/testing-library';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const store = vi.hoisted(() => ({
  openProjectSession: vi.fn(),
  openingSessionId: vi.fn(() => null as string | null),
  projectSessions: vi.fn(() => [{ id: 'hub-abcdef12', projectId: 'p1', title: '新对话', lifecycle: 'ready', updatedAt: '2026-08-13T10:00:00Z', activeChatId: null, acpSessionId: 'acp-12345678' }]),
  projects: vi.fn(() => [{ id: 'p1', name: 'Perihelion', cwd: '/repo', archivedAt: null }]),
  readOnly: vi.fn(() => false),
  selectedSessionId: vi.fn(() => null),
  selectPersistedSessionLocally: vi.fn(),
}));
vi.mock('../store', () => store);

import { SessionSearch } from './SessionSearch';

describe('SessionSearch', () => {
  beforeEach(() => {
    store.openProjectSession.mockReset();
    store.openingSessionId.mockReturnValue(null);
  });

  it('keeps the search context until the exact open commits', async () => {
    const close = vi.fn();
    let callbacks: { onCommitted?: () => void } = {};
    store.openProjectSession.mockImplementation((_id, value) => { callbacks = value; return true; });
    render(() => <SessionSearch open onClose={close} />);
    fireEvent.input(screen.getByRole('textbox', { name: '搜索会话' }), { target: { value: '新对话' } });
    fireEvent.click(await screen.findByRole('option'));
    expect(store.openProjectSession).toHaveBeenCalledWith('hub-abcdef12', expect.any(Object));
    expect(close).not.toHaveBeenCalled();
    expect(screen.getByRole('option')).toHaveTextContent('新对话 · …12345678');
    callbacks.onCommitted?.();
    expect(close).toHaveBeenCalledOnce();
  });

  it('retains the query and explains a definite failure inline', async () => {
    store.openProjectSession.mockImplementation((_id, callbacks) => { callbacks.onFailed('ACP instance offline'); return true; });
    render(() => <SessionSearch open onClose={() => {}} />);
    const input = screen.getByRole('textbox', { name: '搜索会话' });
    fireEvent.input(input, { target: { value: 'Perihelion' } });
    fireEvent.click(await screen.findByRole('option'));
    expect(input).toHaveValue('Perihelion');
    expect(screen.getByRole('alert')).toHaveTextContent('ACP instance offline');
  });

  it('does not imply failure or close when the open result is unknown', async () => {
    const close = vi.fn();
    store.openProjectSession.mockImplementation((_id, callbacks) => { callbacks.onUncertain(); return true; });
    render(() => <SessionSearch open onClose={close} />);
    const input = screen.getByRole('textbox', { name: '搜索会话' });
    fireEvent.input(input, { target: { value: '新对话' } });
    fireEvent.click(await screen.findByRole('option'));
    expect(close).not.toHaveBeenCalled();
    expect(input).toHaveValue('新对话');
    expect(screen.getByRole('alert')).toHaveTextContent('结果尚未确认');
    expect(screen.getByRole('alert')).toHaveTextContent('当前会话没有切换');
  });
});
