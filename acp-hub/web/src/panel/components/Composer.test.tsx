import { fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { afterEach, describe, expect, it } from 'vitest';
import {
  setChatHead,
  setChatStatusSignal,
  setOpeningSession,
  setProjectSessions,
  setRuntimeDocsState,
  setSelectedCid,
  setSelectedSessionId,
} from '../store';
import { setPrincipalRole } from '../lib/auth-state';
import { failMessageDelivery, markMessageDeliveryUncertain, resetMessageDelivery, setComposerDraft, startMessageDelivery } from '../lib/message-delivery';
import { resetRuntimeControls, startRuntimeControl } from '../lib/runtime-control';
import { Composer } from './Composer';

function resetStore() {
  setSelectedCid(null);
  setSelectedSessionId(null);
  setOpeningSession(null);
  setPrincipalRole(null);
  setChatStatusSignal({});
  setChatHead(null);
  resetRuntimeControls();
  setRuntimeDocsState({ chat: false, control: false });
  resetMessageDelivery();
  setProjectSessions([]);
}

function selectReadyChat() {
  setPrincipalRole('full');
  setSelectedSessionId('session-1');
  setSelectedCid('chat-1');
  setChatStatusSignal({ 'chat-1': 'active' });
  setChatHead({ chat: { chatId: 'chat-1', title: 'Chat', status: 'active', activeTurnId: null, createdAt: null, updatedAt: null }, agent: null, activeTurn: null, pendingPermissions: [] });
  setRuntimeDocsState({ chat: true, control: true });
  setProjectSessions([{ id: 'session-1', projectId: 'project-1', acpSessionId: 'acp-1', title: 'Session A', lifecycle: 'ready', updatedAt: null, lastOpenedAt: null, activeChatId: 'chat-1' }]);
}

afterEach(resetStore);

describe('Composer', () => {
  it('does not imply that an unselected disabled editor can accept text', () => {
    setPrincipalRole('full');
    render(() => <Composer />);
    const input = screen.getByRole('textbox');
    expect(input).toBeDisabled();
    expect(input).toHaveAttribute('placeholder', '先从左侧选择或新建会话');
    expect(screen.getByRole('button', { name: '发送' })).toBeDisabled();
  });

  it('enables send only after meaningful input', () => {
    selectReadyChat();
    render(() => <Composer />);
    const input = screen.getByRole('textbox');
    const send = screen.getByRole('button', { name: '发送' });
    expect(input).toHaveAttribute('placeholder', '给 Agent 发消息');
    expect(send).toBeDisabled();
    fireEvent.input(input, { target: { value: '  inspect the state  ' } });
    expect(send).toBeEnabled();
  });

  it('does not imply readiness before both runtime documents arrive', () => {
    selectReadyChat();
    setRuntimeDocsState({ chat: true, control: false });
    render(() => <Composer />);
    expect(screen.getByRole('textbox')).toBeDisabled();
    expect(screen.getByRole('textbox')).toHaveAttribute('placeholder', '正在载入会话…');
    expect(screen.getByRole('button', { name: '发送' })).toBeDisabled();
  });

  it('replaces send with an actionable stop control while a turn is active', () => {
    selectReadyChat();
    setChatHead({ chat: { chatId: 'chat-1', title: 'Chat', status: 'active', activeTurnId: 'turn-1', createdAt: null, updatedAt: null }, agent: null, activeTurn: { turnId: 'turn-1', turnStatus: 'running', updatedAt: null }, pendingPermissions: [] });
    render(() => <Composer />);
    expect(screen.queryByRole('button', { name: '发送' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '停止生成' })).toBeEnabled();
    expect(screen.getByRole('textbox')).toBeDisabled();
  });

  it('keeps stop locked while its exact runtime control is unresolved', () => {
    selectReadyChat();
    setChatHead({ chat: { chatId: 'chat-1', title: 'Chat', status: 'active', activeTurnId: 'turn-1', createdAt: null, updatedAt: null }, agent: null, activeTurn: { turnId: 'turn-1', turnStatus: 'running', updatedAt: null }, pendingPermissions: [] });
    startRuntimeControl('cancel-1', 'chat-1', 'cancel');
    render(() => <Composer />);
    expect(screen.getByRole('button', { name: '停止生成' })).toBeDisabled();
  });

  it('restores uncertain message text and keeps same-request recovery visible', async () => {
    selectReadyChat();
    setComposerDraft('session-1', 'preserved draft');
    startMessageDelivery('cmd-1', 'preserved draft', 'session-1', 'chat-1');
    markMessageDeliveryUncertain('cmd-1');
    render(() => <Composer />);
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('preserved draft'));
    expect(screen.getByRole('alert')).toHaveTextContent('结果尚未确认');
    expect(screen.getByRole('button', { name: '使用同一请求重新确认' })).toBeInTheDocument();
  });

  it('returns a definite failure to editing without promising an invalid same-request retry', async () => {
    selectReadyChat();
    setComposerDraft('session-1', 'fix and send again');
    startMessageDelivery('cmd-failed', 'fix and send again', 'session-1', 'chat-1');
    failMessageDelivery('cmd-failed', '服务器已明确拒绝该请求');
    render(() => <Composer />);
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('fix and send again'));
    expect(screen.getByRole('alert')).toHaveTextContent('消息未发送');
    expect(screen.queryByRole('button', { name: '使用同一请求重新确认' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '返回编辑' })).toBeInTheDocument();
  });

  it('isolates drafts and recovery surfaces by persisted session identity', async () => {
    selectReadyChat();
    render(() => <Composer />);
    const input = screen.getByRole('textbox');
    fireEvent.input(input, { target: { value: 'draft for session A' } });

    setSelectedSessionId('session-2');
    setSelectedCid('chat-2');
    expect(input).toHaveValue('');
    expect(screen.queryByText('draft for session A')).not.toBeInTheDocument();

    fireEvent.input(input, { target: { value: 'draft for session B' } });
    setSelectedSessionId('session-1');
    setSelectedCid('chat-1');
    await waitFor(() => expect(input).toHaveValue('draft for session A'));
  });

  it('never exposes another session message while explaining the global safety gate', () => {
    selectReadyChat();
    setProjectSessions([
      { id: 'session-1', projectId: 'project-1', acpSessionId: 'acp-1', title: 'Session A', lifecycle: 'ready', updatedAt: null, lastOpenedAt: null, activeChatId: 'chat-1' },
      { id: 'session-2', projectId: 'project-1', acpSessionId: 'acp-2', title: 'Session B', lifecycle: 'ready', updatedAt: null, lastOpenedAt: null, activeChatId: 'chat-2' },
    ]);
    startMessageDelivery('cmd-a', 'private draft A', 'session-1', 'chat-1');
    markMessageDeliveryUncertain('cmd-a');
    setSelectedSessionId('session-2');
    setSelectedCid('chat-2');
    render(() => <Composer />);

    expect(screen.getByRole('textbox')).toHaveAttribute('placeholder', '另一会话的消息仍在确认…');
    expect(screen.getByText('另一会话仍在确认')).toBeInTheDocument();
    expect(screen.queryByText('private draft A')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '返回该会话' })).toBeInTheDocument();
  });
});
