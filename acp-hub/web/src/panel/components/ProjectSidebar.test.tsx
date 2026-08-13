import { fireEvent, render, screen } from '@solidjs/testing-library';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const store = vi.hoisted(() => ({
  archiveProject: vi.fn(),
  chatStatusSignal: vi.fn(() => ({})),
  connState: vi.fn(() => ({ kind: 'ok', text: '已连接' })),
  createProject: vi.fn(),
  createProjectSession: vi.fn(),
  creatingSessionProjectId: vi.fn(() => null as string | null),
  importableSessions: vi.fn(() => []),
  importProjectSession: vi.fn(),
  openProjectSession: vi.fn(),
  openingSessionId: vi.fn(() => null as string | null),
  permissions: vi.fn(() => []),
  projects: vi.fn(() => [{
    id: 'p1',
    name: 'Perihelion',
    cwd: '/repo',
    instanceId: 'local',
    createdAt: '2026-08-13T10:00:00Z',
    updatedAt: '2026-08-13T10:00:00Z',
    archivedAt: null,
  }]),
  projectSessions: vi.fn(() => [{
    id: 'hub-abcdef12',
    projectId: 'p1',
    title: '架构重构',
    lifecycle: 'ready',
    updatedAt: '2026-08-13T10:00:00Z',
    lastOpenedAt: null,
    activeChatId: null as string | null,
    acpSessionId: 'acp-12345678',
  }]),
  readOnly: vi.fn(() => false),
  renameProject: vi.fn(),
  renameProjectSession: vi.fn(),
  restoreProject: vi.fn(),
  selectedCid: vi.fn(() => null as string | null),
  selectedSessionId: vi.fn(() => null as string | null),
  selectPersistedSessionLocally: vi.fn(),
  turnActive: vi.fn(() => false),
}));

vi.mock('../store', () => store);
vi.mock('./AuthGate', () => ({ useAuthActions: () => ({ logout: vi.fn() }) }));

import { ProjectSidebar } from './ProjectSidebar';

function sessionButton() {
  return screen.getByRole('button', { name: /^架构重构/ });
}

describe('ProjectSidebar session navigation', () => {
  beforeEach(() => {
    store.openProjectSession.mockReset();
    store.selectPersistedSessionLocally.mockReset();
    store.openingSessionId.mockReturnValue(null);
    store.readOnly.mockReturnValue(false);
    store.projectSessions.mockReturnValue([{
      id: 'hub-abcdef12',
      projectId: 'p1',
      title: '架构重构',
      lifecycle: 'ready',
      updatedAt: '2026-08-13T10:00:00Z',
      lastOpenedAt: null,
      activeChatId: null,
      acpSessionId: 'acp-12345678',
    }]);
  });

  it('waits for the exact open command to commit before navigating', () => {
    const navigate = vi.fn();
    let callbacks: { onCommitted?: () => void; onFailed?: (message: string) => void } = {};
    store.openProjectSession.mockImplementation((_sessionId, value) => {
      callbacks = value;
      return true;
    });

    render(() => <ProjectSidebar onNavigate={navigate} />);
    fireEvent.click(sessionButton());

    expect(store.openProjectSession).toHaveBeenCalledWith('hub-abcdef12', expect.any(Object));
    expect(navigate).not.toHaveBeenCalled();
    callbacks.onCommitted?.();
    expect(navigate).toHaveBeenCalledOnce();
  });

  it('keeps the current navigation context when opening fails', () => {
    const navigate = vi.fn();
    store.openProjectSession.mockImplementation((_sessionId, callbacks) => {
      callbacks.onFailed?.('ACP instance offline');
      return true;
    });

    render(() => <ProjectSidebar onNavigate={navigate} />);
    fireEvent.click(sessionButton());

    expect(navigate).not.toHaveBeenCalled();
  });

  it('lets read-only users switch to an existing runtime without a mutation', () => {
    const navigate = vi.fn();
    store.readOnly.mockReturnValue(true);
    store.projectSessions.mockReturnValue([{
      id: 'hub-abcdef12',
      projectId: 'p1',
      title: '架构重构',
      lifecycle: 'ready',
      updatedAt: '2026-08-13T10:00:00Z',
      lastOpenedAt: null,
      activeChatId: 'chat-live',
      acpSessionId: 'acp-12345678',
    }]);

    render(() => <ProjectSidebar onNavigate={navigate} />);
    fireEvent.click(sessionButton());

    expect(store.selectPersistedSessionLocally).toHaveBeenCalledWith('hub-abcdef12', 'chat-live');
    expect(store.openProjectSession).not.toHaveBeenCalled();
    expect(navigate).toHaveBeenCalledOnce();
  });
});
