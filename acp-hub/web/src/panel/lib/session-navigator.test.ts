import { describe, expect, it, vi } from 'vitest';
import { SessionNavigator } from './session-navigator';

describe('SessionNavigator', () => {
  it('restores only the remembered ready session', () => {
    const navigator = new SessionNavigator();
    expect(navigator.transition({
      type: 'catalog', ready: true, readOnly: false, preferredId: 'ready', selectedSessionId: null,
      sessions: [{ id: 'ready', lifecycle: 'ready' }, { id: 'failed', lifecycle: 'failed' }],
    })).toEqual([{ type: 'request-open', sessionId: 'ready' }]);
    expect(navigator.snapshot().restoringSessionId).toBe('ready');
  });

  it('reuses a proven live runtime without mutation in read-only mode', () => {
    const navigator = new SessionNavigator();
    expect(navigator.transition({
      type: 'catalog', ready: true, readOnly: true, preferredId: 'ready', selectedSessionId: null,
      sessions: [{ id: 'ready', lifecycle: 'ready', activeChatId: 'chat-live' }],
    })).toEqual([{ type: 'activate', sessionId: 'ready', chatId: 'chat-live' }]);
  });

  it('preserves the current selection until the matching terminal acknowledgement', () => {
    const changed = vi.fn();
    const navigator = new SessionNavigator(changed);
    navigator.transition({ type: 'open-started', commandId: 'open-1', sessionId: 'new', previousSessionId: 'old', previousChatId: 'chat-old' });
    expect(navigator.transition({ type: 'open-terminal', commandId: 'other', status: 'committed', chatId: 'chat-wrong' })).toEqual([]);
    expect(navigator.snapshot().opening?.sessionId).toBe('new');
    expect(navigator.transition({ type: 'open-terminal', commandId: 'open-1', status: 'committed', chatId: 'chat-new' }))
      .toEqual([{ type: 'activate', sessionId: 'new', chatId: 'chat-new' }]);
    expect(navigator.snapshot().opening).toBeNull();
    expect(changed).toHaveBeenCalled();
  });

  it('quarantines a terminal acknowledgement after timeout without hiding it from other modules', () => {
    const navigator = new SessionNavigator();
    navigator.transition({ type: 'open-started', commandId: 'late', sessionId: 'new', previousSessionId: 'old', previousChatId: 'chat-old' });
    navigator.transition({ type: 'open-uncertain', commandId: 'late' });
    expect(navigator.transition({ type: 'open-terminal', commandId: 'late', status: 'duplicate', chatId: 'chat-late' })).toEqual([]);
    expect(navigator.snapshot().opening).toBeNull();
  });

  it('does not automatically repeat a failed restore on unrelated catalog updates', () => {
    const navigator = new SessionNavigator();
    const catalog = {
      type: 'catalog' as const, ready: true, readOnly: false, preferredId: 'ready', selectedSessionId: null,
      sessions: [{ id: 'ready', lifecycle: 'ready' }],
    };
    navigator.transition(catalog);
    navigator.transition({ type: 'open-started', commandId: 'restore-1', sessionId: 'ready', previousSessionId: null, previousChatId: null });
    navigator.transition({ type: 'open-failed', commandId: 'restore-1' });
    expect(navigator.transition(catalog)).toEqual([]);
  });

  it('forgets a stale preference exactly once', () => {
    const navigator = new SessionNavigator();
    const event = { type: 'catalog' as const, ready: true, readOnly: false, preferredId: 'missing', selectedSessionId: null, sessions: [] };
    expect(navigator.transition(event)).toEqual([{ type: 'forget-preference' }]);
    expect(navigator.transition(event)).toEqual([]);
  });
});
