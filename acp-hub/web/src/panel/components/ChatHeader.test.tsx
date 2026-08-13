import { fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { afterEach, describe, expect, it } from 'vitest';
import {
  setChatHead,
  setChatStatusSignal,
  setConnState,
  setOpeningSession,
  setPermissions,
  setProjectSessions,
  setRuntimeDocsState,
  setSelectedCid,
  setSelectedSessionId,
} from '../store';
import { ChatHeader } from './ChatHeader';
import { resetRuntimeControls, startRuntimeControl } from '../lib/runtime-control';
import { setPrincipalRole } from '../lib/auth-state';

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
  setRuntimeDocsState({ chat: false, control: false });
  setConnState({ text: '未连接', kind: 'idle' });
  resetRuntimeControls();
  setPrincipalRole(null);
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

  it('disambiguates a fallback title with the durable ACP identity', () => {
    setProjectSessions([{ ...session, title: '新对话', acpSessionId: 'acp-12345678', activeChatId: null }]);
    setSelectedSessionId(session.id);
    render(() => <ChatHeader />);

    expect(screen.getByText('新对话 · …12345678')).toBeInTheDocument();
  });

  it('announces permission attention before generic active work', () => {
    setProjectSessions([session]);
    setSelectedSessionId(session.id);
    setSelectedCid('chat-1');
    setChatStatusSignal({ 'chat-1': 'active' });
    setRuntimeDocsState({ chat: false, control: true });
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

  it('does not announce input readiness before runtime hydration completes', () => {
    setProjectSessions([session]);
    setSelectedSessionId(session.id);
    setSelectedCid('chat-1');
    setChatStatusSignal({ 'chat-1': 'active' });
    setRuntimeDocsState({ chat: true, control: false });
    render(() => <ChatHeader />);

    expect(screen.getByText('正在载入会话…')).toBeInTheDocument();
    expect(screen.queryByText('可输入 · 会话已保存')).not.toBeInTheDocument();
  });

  it('prevents close while another control owns the runtime', () => {
    setProjectSessions([session]);
    setSelectedSessionId(session.id);
    setSelectedCid('chat-1');
    setChatStatusSignal({ 'chat-1': 'active' });
    startRuntimeControl('cancel-1', 'chat-1', 'cancel');
    render(() => <ChatHeader />);
    screen.getByRole('button', { name: '会话操作' }).click();
    expect(screen.getByRole('menuitem', { name: '关闭运行实例' })).toBeDisabled();
  });

  it('closes the confirmation when Registry proves the runtime is terminal', async () => {
    setPrincipalRole('full');
    setProjectSessions([session]);
    setSelectedSessionId(session.id);
    setSelectedCid('chat-1');
    setChatStatusSignal({ 'chat-1': 'active' });
    render(() => <ChatHeader />);
    fireEvent.click(screen.getByRole('button', { name: '会话操作' }));
    fireEvent.click(screen.getByRole('menuitem', { name: '关闭运行实例' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    setChatStatusSignal({ 'chat-1': 'closed' });
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });
});
