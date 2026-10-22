import { afterEach, describe, expect, test } from 'bun:test';
import type { ChatShellChat } from './ChatShell';
import { installDom } from '../../test/dom';

installDom();

const { cleanup, fireEvent, render, screen, within, act } = await import('@testing-library/react');
const { ChatShellView } = await import('./ChatShell');
const { errorBanners } = await import('../runtime');
const { createElement, useRef, useState } = await import('react');

afterEach(() => cleanup());

const summary = (id: string, title = id) => ({
  id, title, updatedAt: '2026-10-20T00:00:00Z',
  messageCount: 0, running: false, pendingInteractionCount: 0,
  lastTurnStatus: null, lastCompletedAt: null,
});

const emptyState = (id: string) => ({
  chat: summary(id), running: false, executionBlocked: false, readiness: 'ready', revision: 0,
  entries: [], toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
  session: {}, tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
  pendingInteractions: [], inputQueue: null,
});

interface FakeOptions {
  initialId?: string | null;
}

type ChatFacade = ChatShellChat;

function fakeChat(options: FakeOptions = {}) {
  const ref = { id: options.initialId ?? null };
  const calls = { reload: 0, refresh: 0, selected: [] as (string | null)[] };
  const selected = () => (ref.id ? summary(ref.id) : null);
  return {
    ref,
    calls,
    chat: {
      selected: selected(), state: null, phase: 'idle', historyReady: true,
      loadState: 'ready', liveSync: 'synced', error: '', chatError: '', listError: '',
      chats: [], listLoading: false, banners: {},
      payloadReader: {}, answerInteraction: async () => {},
      refreshList: async () => { calls.refresh += 1; },
      selectChat: async () => { calls.selected.push(null); return true; },
      reloadChat: async () => { calls.reload += 1; return false; },
      currentChatId: () => ref.id,
      send: async () => ref.id ?? null,
      stop: async () => true,
      dismissError: () => {}, dismissChatError: () => {}, dismissListError: () => {},
    } as unknown as ChatFacade,
  };
}

/** 有状态外壳：选择/发送会同步更新 currentChatId，供草稿迁移测试。 */
function Harness({
  initialId = null, nextId = 'chat-created', selectOpened = true, createFailsOnce = false,
  sendFails = false, existingChats = [],
}: {
  initialId?: string | null; nextId?: string | null; selectOpened?: boolean; createFailsOnce?: boolean;
  /** 模拟隐式创建成功但 prompt 失败。 */
  sendFails?: boolean;
  /** 已存在的会话：用于验证草稿槽互不污染。 */
  existingChats?: readonly string[];
}) {
  const [selectedId, setSelectedId] = useState<string | null>(initialId);
  const [createdIds, setCreatedIds] = useState<readonly string[]>([]);
  const [failed, setFailed] = useState(false);
  const selectedRef = useRef<string | null>(initialId);
  const calls = useRef({ reload: 0, selected: 0 }).current;
  const created = useRef(0);
  const attempts = useRef(0);
  const createId = () => {
    const id = nextId === null ? null : `${nextId}-${++created.current}`;
    if (id) setCreatedIds((previous) => [...previous, id]);
    return id;
  };

  const choose = (id: string | null, opened = selectOpened): boolean => {
    selectedRef.current = id;
    setSelectedId(id);
    // opened=false 模拟创建成功但 read/watch 失败：会话已存在，读面仍失败。
    if (!opened) {
      setFailed(true);
      return false;
    }
    setFailed(false);
    return id !== null;
  };

  const chat = {
    ...fakeChat().chat,
    chats: [...existingChats, ...createdIds].map((id) => summary(id)),
    selected: selectedId ? summary(selectedId) : null,
    historyReady: !failed,
    loadState: failed ? 'failed' : 'ready',
    chatError: failed ? '创建失败' : '',
    error: failed ? '创建失败' : '',
    banners: errorBanners({ listError: '', chatError: failed ? '创建失败' : '', loadState: failed ? 'failed' : 'ready' }),
    currentChatId: () => selectedRef.current,
    selectChat: async (target: { id: string } | null) => {
      calls.selected += 1;
      if (target === null && createFailsOnce && attempts.current++ === 0) return choose(null, false);
      return choose(target?.id ?? createId());
    },
    reloadChat: async () => {
      calls.reload += 1;
      return choose(createId());
    },
    send: async () => {
      const id = createId();
      choose(id);
      return sendFails ? null : id;
    },
  } as ChatFacade;

  return createElement(ChatShellView, { chat, onOpenSettings: () => {} });
}

describe('ChatShell real DOM recovery and panel interactions', () => {
  test('list and chat errors render separate banners with matching retry actions', () => {
    const base = fakeChat();
    const chat = {
      ...base.chat,
      listError: '列表失败',
      loadState: 'failed' as const,
      chatError: '会话读取失败',
      banners: errorBanners({ listError: '列表失败', chatError: '会话读取失败', loadState: 'failed' }),
    };
    render(createElement(ChatShellView, { chat: chat as ChatFacade, onOpenSettings: () => {} }));

    expect(screen.getByText('列表失败')).toBeTruthy();
    expect(screen.getByText('会话读取失败')).toBeTruthy();
    fireEvent.click(screen.getByText('刷新列表'));
    const chatBanner = document.querySelector('[data-slot="peri-error-banner"]')!;
    fireEvent.click(within(chatBanner as HTMLElement).getByText('重新加载'));
    expect(base.calls.refresh).toBe(1);
    expect(base.calls.reload).toBe(1);
    // 读面失败横幅不可关闭；列表横幅有独立关闭入口。
    expect(screen.getByLabelText('关闭列表错误提示')).toBeTruthy();
    expect(screen.queryByLabelText('关闭错误提示')).toBeNull();
  });

  test('execution errors have no reload retry and can be dismissed', () => {
    const base = fakeChat();
    let dismissed = 0;
    const chat = {
      ...base.chat,
      chatError: '本轮执行失败',
      error: '本轮执行失败',
      banners: errorBanners({ listError: '', chatError: '本轮执行失败', loadState: 'ready' }),
      dismissChatError: () => { dismissed += 1; },
    };
    render(createElement(ChatShellView, { chat: chat as ChatFacade, onOpenSettings: () => {} }));

    expect(screen.getByText('本轮执行失败')).toBeTruthy();
    expect(screen.queryByText('重新加载')).toBeNull();
    fireEvent.click(screen.getByLabelText('关闭错误提示'));
    expect(dismissed).toBe(1);
  });

  test('a live sync disconnect offers reconnect when content is already readable', () => {
    const base = fakeChat({ initialId: 'chat-a' });
    const chat = {
      ...base.chat,
      liveSync: 'failed' as const,
      state: emptyState('chat-a') as never,
    };
    render(createElement(ChatShellView, { chat: chat as ChatFacade, onOpenSettings: () => {} }));
    expect(screen.getByText('同步失败')).toBeTruthy();
    fireEvent.click(screen.getByText('重新连接'));
    expect(base.calls.reload).toBe(1);
  });

  test('the details panel opens and closes through its header button', async () => {
    const base = fakeChat({ initialId: 'chat-a' });
    render(createElement(ChatShellView, { chat: base.chat as ChatFacade, onOpenSettings: () => {} }));
    const panel = document.getElementById('peri-right-panel')!;
    expect(panel.getAttribute('aria-hidden')).toBe('true');
    fireEvent.click(screen.getByLabelText('详情面板'));
    expect(panel.getAttribute('aria-hidden')).toBe('false');
    fireEvent.click(screen.getByLabelText('关闭详情面板'));
    expect(panel.getAttribute('aria-hidden')).toBe('true');
  });

  test('right panel tabs reuse plan and task facts and expose tab semantics', () => {
    const base = fakeChat({ initialId: 'chat-a' });
    const state = {
      ...emptyState('chat-a'),
      plans: { current: [{ content: '实现右面板', status: 'completed' }], byTurn: {} },
      tasks: [{ taskId: 'task-1', kind: 'tool', title: '运行测试', status: 'running' }],
    };
    const chat = { ...base.chat, state: state as never };
    render(createElement(ChatShellView, { chat: chat as ChatFacade, onOpenSettings: () => {} }));
    fireEvent.click(screen.getByLabelText('详情面板'));

    const tab = (label: string) => screen.getByRole('tab', { name: label });
    expect(tab('详情').getAttribute('aria-selected')).toBe('true');
    fireEvent.click(tab('计划'));
    expect(tab('计划').getAttribute('aria-selected')).toBe('true');
    expect(screen.getByText('实现右面板')).toBeTruthy();
    fireEvent.click(tab('任务'));
    expect(screen.getByText('运行测试')).toBeTruthy();
    expect(screen.queryByText('实现右面板')).toBeNull();
  });

  test('narrow sheet moves focus in, traps Tab and returns focus to the trigger', () => {
    const original = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      // ChatRightPanel 查询 `(min-width: 981px)`；返回 false 即窄屏 overlay。
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => true,
    })) as typeof window.matchMedia;
    try {
      const base = fakeChat({ initialId: 'chat-a' });
      render(createElement(ChatShellView, { chat: base.chat as ChatFacade, onOpenSettings: () => {} }));
      const trigger = screen.getByLabelText('详情面板');
      trigger.focus();
      fireEvent.click(trigger);
      const panel = document.getElementById('peri-right-panel')!;
      expect(panel.getAttribute('role')).toBe('dialog');
      expect(panel.getAttribute('aria-modal')).toBe('true');
      // 打开后焦点进入面板（第一个 tab）。
      expect(document.activeElement?.textContent).toBe('详情');

      // 从第一个可聚焦元素 Shift+Tab 回到最后一个（关闭按钮），Tab 再回到第一个。
      fireEvent.keyDown(panel, { key: 'Tab', shiftKey: true });
      expect(document.activeElement?.getAttribute('aria-label')).toBe('关闭详情面板');
      fireEvent.keyDown(panel, { key: 'Tab' });
      expect(document.activeElement?.textContent).toBe('详情');

      fireEvent.click(screen.getByLabelText('关闭详情面板'));
      expect(document.activeElement).toBe(trigger);
    } finally {
      window.matchMedia = original;
    }
  });
});

describe('ChatShell draft migration through real input events', () => {
  test('a normal new-chat selection migrates the draft and clears the placeholder', async () => {
    render(createElement(Harness, { nextId: 'chat-new-1' }));
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: '新建期间的草稿' } });
    await act(async () => { fireEvent.click(screen.getByText('新建会话')); });
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('新建期间的草稿');

    await act(async () => { fireEvent.click(screen.getByText('新建会话')); });
    // 旧草稿已随第一次创建迁走并清空占位槽，不被下一次新会话继承。
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('');
  });

  test('creation success with a failed read still migrates the draft', async () => {
    render(createElement(Harness, { nextId: 'chat-broken', selectOpened: false }));
    fireEvent.change(screen.getByLabelText('输入消息'), { target: { value: '失败后仍在' } });
    await act(async () => { fireEvent.click(screen.getByText('新建会话')); });
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('失败后仍在');
    // 失败读面保留重试入口，草稿只在一个真实会话槽中。
    const banner = document.querySelector('[data-slot="peri-error-banner"]')!;
    expect(within(banner as HTMLElement).getByText('重新加载')).toBeTruthy();
  });

  test('the failure banner retry migrates the draft after creation finally succeeds', async () => {
    render(createElement(Harness, { nextId: 'chat-retried', createFailsOnce: true }));
    fireEvent.change(screen.getByLabelText('输入消息'), { target: { value: '重试保留' } });
    await act(async () => { fireEvent.click(screen.getByText('新建会话')); });
    // 创建失败：保留占位草稿和可恢复入口。
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('重试保留');
    const banner = document.querySelector('[data-slot="peri-error-banner"]')!;
    expect(within(banner as HTMLElement).getByText('重新加载')).toBeTruthy();

    await act(async () => {
      fireEvent.click(within(banner as HTMLElement).getByText('重新加载'));
    });
    // 重试创建成功：草稿迁到真实会话，占位槽清空。
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('重试保留');
    expect(document.querySelector('[data-slot="peri-error-banner"]')).toBeNull();
  });

  test('implicit send creates a chat, migrates and clears the submitted draft', async () => {
    render(createElement(Harness, { nextId: 'chat-from-send' }));
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: '你好' } });
    await act(async () => { fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 }); });
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('');
  });

  test('a failed prompt after implicit creation keeps the draft in the created chat only', async () => {
    render(createElement(Harness, {
      nextId: 'chat-prompt-failed', sendFails: true, existingChats: ['chat-b'],
    }));
    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: '不要丢' } });
    await act(async () => { fireEvent.keyDown(textarea, { key: 'Enter', code: 'Enter', keyCode: 13 }); });

    // 创建成功、prompt 失败：当前会话输入框仍保留刚提交的文本。
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('不要丢');

    // 另一个会话的草稿槽独立：B 写入后切回创建出的会话，两边互不覆盖。
    await act(async () => { fireEvent.click(screen.getByText('chat-b')); });
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('');
    fireEvent.change(screen.getByLabelText('输入消息'), { target: { value: 'B 草稿' } });
    await act(async () => { fireEvent.click(screen.getByText('chat-prompt-failed-1')); });
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('不要丢');
    await act(async () => { fireEvent.click(screen.getByText('chat-b')); });
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('B 草稿');
  });
});
