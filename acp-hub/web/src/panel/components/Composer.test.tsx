import { fireEvent, render, screen, waitFor } from '@solidjs/testing-library';
import { afterEach, describe, expect, it } from 'vitest';
import {
  setCancellingTurn,
  setChatHead,
  setChatStatusSignal,
  setMessageSubmission,
  setOpeningSession,
  setPrincipalRole,
  setSelectedCid,
} from '../store';
import { Composer } from './Composer';

function resetStore() {
  setSelectedCid(null);
  setOpeningSession(null);
  setPrincipalRole(null);
  setChatStatusSignal({});
  setChatHead(null);
  setMessageSubmission(null);
  setCancellingTurn(false);
}

function selectReadyChat() {
  setPrincipalRole('full');
  setSelectedCid('chat-1');
  setChatStatusSignal({ 'chat-1': 'active' });
  setChatHead({ chat: { chatId: 'chat-1', title: 'Chat', status: 'active', activeTurnId: null, createdAt: null, updatedAt: null }, agent: null, activeTurn: null, pendingPermissions: [] });
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

  it('replaces send with an actionable stop control while a turn is active', () => {
    selectReadyChat();
    setChatHead({ chat: { chatId: 'chat-1', title: 'Chat', status: 'active', activeTurnId: 'turn-1', createdAt: null, updatedAt: null }, agent: null, activeTurn: { turnId: 'turn-1', turnStatus: 'running', updatedAt: null }, pendingPermissions: [] });
    render(() => <Composer />);
    expect(screen.queryByRole('button', { name: '发送' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '停止生成' })).toBeEnabled();
    expect(screen.getByRole('textbox')).toBeDisabled();
  });

  it('restores uncertain message text and keeps same-request recovery visible', async () => {
    selectReadyChat();
    setMessageSubmission({ commandId: 'cmd-1', text: 'preserved draft', phase: 'uncertain', detail: '服务器尚未确认', retryable: true });
    render(() => <Composer />);
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('preserved draft'));
    expect(screen.getByRole('alert')).toHaveTextContent('结果尚未确认');
    expect(screen.getByRole('button', { name: '使用同一请求重新确认' })).toBeInTheDocument();
  });
});
