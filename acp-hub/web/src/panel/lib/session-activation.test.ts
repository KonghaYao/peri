import { afterEach, describe, expect, it, vi } from 'vitest';
import { resetQuickStart } from './quick-start-delivery';
import { SessionActivation, type ActivationSendOptions } from './session-activation';

interface HarnessOverrides {
  ready?: boolean;
  readOnly?: boolean;
  uncertain?: boolean;
  messagePending?: boolean;
  creatingProjectId?: string | null;
  sessions?: Array<{ id: string; projectId: string; acpSessionId: string | null; title: string; lifecycle: string; updatedAt: string | null; lastOpenedAt: string | null; activeChatId: string | null; archivedAt?: string | null }>;
}

function harness(overrides: HarnessOverrides = {}) {
  let creatingProjectId = overrides.creatingProjectId ?? null;
  let sendOptions: ActivationSendOptions | null = null;
  let sentFrame: Record<string, unknown> | null = null;
  const activate = vi.fn();
  const persistProblem = vi.fn();
  const toast = vi.fn();
  const sendFirstMessage = vi.fn(() => true);
  const activation = new SessionActivation({
    isReady: () => overrides.ready ?? true,
    isReadOnly: () => overrides.readOnly ?? false,
    hasUncertainMetadata: () => overrides.uncertain ?? false,
    hasMessageSubmission: () => overrides.messagePending ?? false,
    creatingProjectId: () => creatingProjectId,
    setCreatingProjectId: (value) => { creatingProjectId = value; },
    sessions: () => overrides.sessions ?? [],
    selectedSessionId: () => 'old-session',
    currentChatId: () => 'old-chat',
    preferredSessionId: () => null,
    send: (frame, _label, options) => { sentFrame = frame; sendOptions = options; return true; },
    retry: () => 'sent',
    hasUncertainCommand: () => true,
    activate,
    forgetPreference: vi.fn(),
    sendFirstMessage,
    onNavigationChange: vi.fn(),
    toast,
    persistProblem,
  });
  return {
    activation,
    activate,
    persistProblem,
    toast,
    sendFirstMessage,
    sentFrame: () => sentFrame as { commandId: string; payload: Record<string, unknown> } | null,
    options: () => sendOptions as ActivationSendOptions | null,
    creatingProjectId: () => creatingProjectId,
  };
}

afterEach(resetQuickStart);

describe('SessionActivation', () => {
  it('fails closed at one creation gate before constructing transport work', () => {
    for (const [overrides, message] of [
      [{ ready: false }, '连接未就绪'],
      [{ readOnly: true }, '只读模式不能创建会话'],
      [{ uncertain: true }, '先处理“结果尚未确认”的项目或会话操作'],
      [{ creatingProjectId: 'busy' }, '已有会话正在创建'],
    ] as const) {
      const subject = harness(overrides);
      expect(subject.activation.create('project')).toBe(false);
      expect(subject.sentFrame()).toBeNull();
      expect(subject.toast).toHaveBeenCalledWith(message);
    }
  });

  it('activates an empty session only after a complete committed identity', () => {
    const subject = harness();
    expect(subject.activation.create('project')).toBe(true);
    const commandId = subject.sentFrame()!.commandId;
    expect(subject.creatingProjectId()).toBe('project');

    subject.options()!.cb?.({ commandId, status: 'accepted' });
    expect(subject.activate).not.toHaveBeenCalled();
    subject.options()!.cb?.({ commandId, status: 'committed', sessionId: 'logical' });
    expect(subject.activate).not.toHaveBeenCalled();
    expect(subject.persistProblem).toHaveBeenCalledWith(
      '创建会话回复不完整', expect.stringContaining('页面没有切换'), commandId,
    );
    expect(subject.creatingProjectId()).toBeNull();
  });

  it('keeps open selection atomic and quarantines a late terminal after timeout', () => {
    const subject = harness({ sessions: [{
      id: 'logical', projectId: 'project', acpSessionId: 'acp', title: 'title', lifecycle: 'ready',
      updatedAt: null, lastOpenedAt: null, activeChatId: null,
    }] });
    const uncertain = vi.fn();
    expect(subject.activation.navigate('logical', { onUncertain: uncertain })).toBe(true);
    const commandId = subject.sentFrame()!.commandId;
    subject.options()!.onTimeout?.();
    expect(uncertain).toHaveBeenCalledOnce();
    expect(subject.activate).not.toHaveBeenCalled();
    subject.options()!.cb?.({ commandId, status: 'committed', sessionId: 'logical', chatId: 'late-chat' });
    expect(subject.activate).not.toHaveBeenCalled();
  });

  it('quick start activates then submits the exact preserved source text', () => {
    const subject = harness();
    expect(subject.activation.quickStart('project', '  第一行 🚀\n第二行  ')).toBe(true);
    const frame = subject.sentFrame()!;
    expect(frame.payload.title).toBe('第一行 🚀');
    subject.options()!.cb?.({ commandId: frame.commandId, status: 'duplicate', sessionId: 'logical', chatId: 'chat' });
    expect(subject.activate).toHaveBeenCalledWith('logical', 'chat');
    expect(subject.sendFirstMessage).toHaveBeenCalledWith('第一行 🚀\n第二行');
  });

  it('uses projection-proven runtime locally in read-only mode without sending open', () => {
    const subject = harness({ readOnly: true, sessions: [{
      id: 'logical', projectId: 'project', acpSessionId: 'acp', title: 'title', lifecycle: 'ready',
      updatedAt: null, lastOpenedAt: null, activeChatId: 'live-chat',
    }] });
    expect(subject.activation.navigate('logical')).toBe(true);
    expect(subject.sentFrame()).toBeNull();
    expect(subject.activate).toHaveBeenCalledWith('logical', 'live-chat');
  });
});
