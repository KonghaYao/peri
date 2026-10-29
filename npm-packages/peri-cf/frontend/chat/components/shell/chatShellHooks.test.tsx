import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../../test/dom';

installDom();

const { act, cleanup, renderHook } = await import('@testing-library/react');
const { useComposerDrafts } = await import('./useComposerDrafts');
const { useChatNavigation } = await import('./useChatNavigation');

afterEach(() => cleanup());
beforeEach(() => {
  try {
    window.localStorage.clear();
  } catch {
    // 测试环境没有 localStorage 时，草稿持久化用例会退化为内存模式。
  }
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function baseChat() {
  return {
    selectChat: async () => true,
    reloadChat: async () => true,
    rebuildChat: async () => true,
    refreshList: async () => {},
    currentChatId: () => 'chat-a',
    send: async () => ({ delivered: true as const, sessionId: 'chat-a' }),
  };
}

const summary = (sessionId: string) => ({
  sessionId, title: sessionId, cwd: '/workspace', messageCount: 0,
  createdAt: '2026-10-20T00:00:00Z', updatedAt: '2026-10-20T00:00:00Z',
});

describe('useComposerDrafts', () => {
  test('draft slots are isolated per session id', () => {
    const { result, rerender } = renderHook(
      ({ id }: { id: string | null }) => useComposerDrafts(id, () => id),
      { initialProps: { id: 'chat-a' as string | null } },
    );

    act(() => { result.current.changeDraft('A 草稿'); });
    rerender({ id: 'chat-b' });
    expect(result.current.draft).toBe('');

    act(() => { result.current.changeDraft('B 草稿'); });
    rerender({ id: 'chat-a' });
    expect(result.current.draft).toBe('A 草稿');

    rerender({ id: 'chat-b' });
    expect(result.current.draft).toBe('B 草稿');
  });

  test('the placeholder slot is used while creating and migrated afterwards', () => {
    let createdId: string | null = null;
    const { result, rerender } = renderHook(
      ({ id }: { id: string | null }) => useComposerDrafts(id, () => createdId),
      { initialProps: { id: null as string | null } },
    );

    act(() => { result.current.beginNavigation(true); });
    act(() => { result.current.changeDraft('创建期间输入'); });
    expect(result.current.draftKey).toBe('__new__');

    createdId = 'chat-created';
    rerender({ id: 'chat-created' });
    act(() => { result.current.finishNavigation(); });
    act(() => { result.current.adoptCreatedDraft(); });
    expect(result.current.draft).toBe('创建期间输入');
  });

  test('a successful send clears the submitted draft from the migrated slot', () => {
    let createdId: string | null = null;
    const { result, rerender } = renderHook(
      ({ id }: { id: string | null }) => useComposerDrafts(id, () => createdId),
      { initialProps: { id: null as string | null } },
    );

    act(() => { result.current.changeDraft('你好'); });
    act(() => { result.current.beginImplicitCreate(); });
    createdId = 'chat-from-send';
    rerender({ id: 'chat-from-send' });

    const settled: { delivered: string | null } = { delivered: null };
    act(() => {
      settled.delivered = result.current.settleSentDraft({
        content: '你好',
        selectedBefore: null,
        outcome: { delivered: true, sessionId: 'chat-from-send' },
        navigationStillCurrent: true,
      });
    });
    expect(settled.delivered).toBe('chat-from-send');
    expect(result.current.draft).toBe('');
  });

  test('a failed send keeps the draft in the created chat', () => {
    let createdId: string | null = 'chat-created';
    const { result } = renderHook(() => useComposerDrafts('chat-created', () => createdId));

    act(() => { result.current.changeDraft('不要丢'); });
    act(() => {
      result.current.settleSentDraft({
        content: '不要丢',
        selectedBefore: null,
        outcome: { delivered: false, sessionId: 'chat-created', message: '未送达' },
        navigationStillCurrent: true,
      });
    });
    expect(result.current.draft).toBe('不要丢');
  });

  test('typing does not rebuild navigation callbacks', () => {
    const chat = baseChat();
    const { result } = renderHook(() => {
      const drafts = useComposerDrafts('chat-a', chat.currentChatId);
      const navigation = useChatNavigation(chat, drafts);
      return { drafts, navigation };
    });
    const send = result.current.navigation.send;
    const runAction = result.current.navigation.runAction;

    act(() => { result.current.drafts.changeDraft('你好'); });
    expect(result.current.drafts.draft).toBe('你好');
    expect(result.current.navigation.send).toBe(send);
    expect(result.current.navigation.runAction).toBe(runAction);
  });

  test('persists drafts per session, debounces writes and clears on request', async () => {
    const first = renderHook(() => useComposerDrafts('chat-persist', () => 'chat-persist'));
    act(() => { first.result.current.changeDraft('持久草稿'); });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 350));
    });
    first.unmount();

    const second = renderHook(() => useComposerDrafts('chat-persist', () => 'chat-persist'));
    expect(second.result.current.draft).toBe('持久草稿');

    act(() => { second.result.current.clearPersistedDrafts(); });
    expect(second.result.current.draft).toBe('');
    second.unmount();

    const third = renderHook(() => useComposerDrafts('chat-persist', () => 'chat-persist'));
    expect(third.result.current.draft).toBe('');
    third.unmount();
  });
});

describe('useChatNavigation', () => {
  test('a superseded selection never grabs focus', async () => {
    const first = deferred<boolean>();
    const second = deferred<boolean>();
    let calls = 0;
    const chat = {
      ...baseChat(),
      selectChat: () => (++calls === 1 ? first.promise : second.promise),
    };
    const { result } = renderHook(() => {
      const drafts = useComposerDrafts(null, chat.currentChatId);
      return useChatNavigation(chat, drafts);
    });

    act(() => { void result.current.select(null); });
    act(() => { void result.current.select(summary('chat-b')); });

    await act(async () => {
      second.resolve(true);
      await second.promise;
    });
    expect(result.current.focusSignal).toBe(1);

    await act(async () => {
      first.resolve(true);
      await first.promise;
    });
    expect(result.current.focusSignal).toBe(1);
  });

  test('a send settles the draft through the real outcome', async () => {
    const chat = {
      ...baseChat(),
      send: async () => ({ delivered: true as const, sessionId: 'chat-a' }),
    };
    const { result } = renderHook(() => {
      const drafts = useComposerDrafts('chat-a', chat.currentChatId);
      const navigation = useChatNavigation(chat, drafts);
      return { drafts, navigation };
    });

    await act(async () => {
      await result.current.navigation.send('你好');
    });
    expect(result.current.navigation.scrollSignal).toEqual({ token: 1, chatId: 'chat-a' });
  });
});
