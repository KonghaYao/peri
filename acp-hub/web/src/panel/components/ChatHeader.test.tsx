import { render, screen } from '@solidjs/testing-library';
import { afterEach, describe, expect, it } from 'vitest';
import {
  setChatHead,
  setChatStatusSignal,
  setConnState,
  setOpeningSession,
  setPermissions,
  setProjectSessions,
  setSelectedCid,
  setSelectedSessionId,
} from '../store';
import { ChatHeader } from './ChatHeader';

const session = {
  id: 'session-1',
  projectId: 'project-1',
  acpSessionId: 'acp-1',
  title: '持久会话',
  lifecycle: 'ready',
  updatedAt: null,
  lastOpenedAt: null,
  activeChatId: 'chat-1',
};

function resetStore() {
  setProjectSessions([]);
  setSelectedSessionId(null);
  setSelectedCid(null);
  setOpeningSession(null);
  setChatStatusSignal({});
  setChatHead(null);
  setPermissions([]);
  setConnState({ text: '未连接', kind: 'idle' });
}

afterEach(resetStore);

describe('ChatHeader runtime truth', () => {
  it('keeps durable session identity separate from an absent runtime', () => {
    setProjectSessions([{ ...session, activeChatId: null }]);
    setSelectedSessionId(session.id);
    render(() => <ChatHeader />);

    expect(screen.getByText('持久会话')).toBeInTheDocument();
    expect(screen.getByText('未启动 · 会话已保存')).toBeInTheDocument();
  });

  it('announces permission attention before generic active work', () => {
    setProjectSessions([session]);
    setSelectedSessionId(session.id);
    setSelectedCid('chat-1');
    setChatStatusSignal({ 'chat-1': 'active' });
    setChatHead({
      chat: { chatId: 'chat-1', title: session.title, status: 'active', activeTurnId: 'turn-1', createdAt: null, updatedAt: null },
      agent: null,
      activeTurn: { turnId: 'turn-1', turnStatus: 'running', updatedAt: null },
      pendingPermissions: [],
    });
    setPermissions([{ permissionId: 'permission-1', turnId: 'turn-1', toolCallId: 'tool-1', title: 'Shell', description: null, status: 'pending', expiresAt: null, decision: null }]);
    render(() => <ChatHeader />);

    expect(screen.getByText('等待你的许可')).toBeInTheDocument();
    expect(screen.queryByText('Agent 正在工作')).not.toBeInTheDocument();
  });

  it('states that a crashed runtime did not delete the session', () => {
    setProjectSessions([session]);
    setSelectedSessionId(session.id);
    setSelectedCid('chat-1');
    setChatStatusSignal({ 'chat-1': 'crashed' });
    render(() => <ChatHeader />);

    const status = screen.getByText('运行异常退出 · 会话已保留');
    expect(status).toHaveClass('runtime-status--danger');
  });
});
