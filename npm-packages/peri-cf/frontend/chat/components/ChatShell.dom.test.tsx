import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import type { ChatShellChat } from './ChatShell';
import { installDom } from '../../test/dom';

installDom();

const { cleanup, fireEvent, render, screen, within, act, waitFor } = await import('@testing-library/react');
const { ChatShellView } = await import('./ChatShell');
const { errorBanners } = await import('../runtime');
const { createElement, useRef, useState } = await import('react');

afterEach(() => cleanup());
beforeEach(() => {
  try {
    window.localStorage.clear();
  } catch {
    // 没有 localStorage 的环境不需要隔离持久化草稿。
  }
});

/** ChatShell 的 Timeline 使用 LegendList：给 happy-dom 一个最小可滚动布局。 */
function stubShellLayout(): void {
  class ImmediateResizeObserver {
    constructor(private readonly callback: (entries: unknown[], observer: unknown) => void) {}
    observe(element: Element): void {
      this.callback([{ target: element, contentRect: element.getBoundingClientRect() }], this);
    }
    unobserve(): void {}
    disconnect(): void {}
  }
  Object.defineProperty(globalThis, 'ResizeObserver', {
    value: ImmediateResizeObserver, configurable: true, writable: true,
  });
  HTMLElement.prototype.getBoundingClientRect = function getRect(this: HTMLElement) {
    if (this.hasAttribute('data-timeline-row-kind') || this.hasAttribute('data-timeline-row-id') ||
      this.querySelector(':scope > [data-timeline-row-kind]')) {
      return new DOMRect(0, 0, 760, 120);
    }
    if (this.classList.contains('legend-list-content-container')) return new DOMRect(0, 0, 760, 2_000);
    return new DOMRect(0, 0, 800, 600);
  };
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) { return this.getBoundingClientRect().height; },
  });
  Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
    configurable: true,
    get(this: HTMLElement) {
      if (this.classList.contains('legend-list-content-container')) return 2_000;
      if (this.getAttribute('data-slot') === 'peri-timeline') return 4_000;
      return 600;
    },
  });
  HTMLElement.prototype.scrollTo = function scrollTo(
    this: HTMLElement, options?: ScrollToOptions | number, y?: number,
  ): void {
    if (typeof options === 'object' && options !== null && typeof options.top === 'number') {
      this.scrollTop = options.top;
    } else if (typeof options === 'number' && typeof y === 'number') {
      this.scrollLeft = options;
      this.scrollTop = y;
    }
  } as typeof HTMLElement.prototype.scrollTo;
}

/** 单个 assistant 文本块；用于 OpenUI action 路由测试。 */
function openUIState(text: string, chatId = 'chat-openui') {
  return {
    chat: { id: chatId, title: 'OpenUI', updatedAt: '2026-10-20T00:00:00Z' },
    sessionId: chatId, bindingRevision: 1, running: false, executionBlocked: false, readiness: 'ready',
    entries: [{
      entryId: 'entry:openui', id: 'entry:openui', kind: 'message', turnId: 'turn:openui',
      role: 'assistant', status: 'completed', createdAt: '2026-10-20T00:00:00Z',
      createdAtSource: 'observed', completedAt: '2026-10-20T00:00:01Z', error: null,
      blocks: [{ blockId: 'text:0', type: 'text', text }],
    }],
    toolCalls: [], turns: [{
      turnId: 'turn:openui', status: 'completed', startedAt: '2026-10-20T00:00:00Z',
      completedAt: '2026-10-20T00:00:01Z', error: null, interactionStatus: 'idle',
    }],
    activeTurn: { turnId: null, status: null }, session: {},
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
    pendingInteractions: [], inputQueue: null,
  };
}

const summary = (sessionId: string, title = sessionId) => ({
  sessionId, title, updatedAt: '2026-10-20T00:00:00Z',
  cwd: '/workspace', messageCount: 0, createdAt: '2026-10-20T00:00:00Z',
  projectId: 'default',
});

/** 展示身份 `Chat` 由 `sessionId` 派生，和 `toChat` 一致。 */
const selectedChat = (sessionId: string, title = sessionId) => ({
  id: sessionId, title, updatedAt: '2026-10-20T00:00:00Z',
});

const emptyState = (id: string) => ({
  chat: { id, title: id, updatedAt: '2026-10-20T00:00:00Z' },
  sessionId: id, bindingRevision: 0, running: false, executionBlocked: false, readiness: 'ready',
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
  const calls = { reload: 0, rebuild: 0, refresh: 0, selected: [] as (string | null)[] };
  const selected = () => (ref.id ? selectedChat(ref.id) : null);
  return {
    ref,
    calls,
    chat: {
      selected: selected(), state: null, phase: 'idle', historyReady: true,
      loadState: 'ready', liveSync: 'synced', error: '', chatError: '', listError: '',
      chats: [], listLoading: false, banners: {},
      projects: [], projectsLoading: false,
      createProject: async (name: string) => ({
        id: `project-${name}`, name, createdAt: '2026-10-20T00:00:00Z', isDefault: false,
      }),
      payloadReader: {}, answerInteraction: async () => {},
      refreshList: async () => { calls.refresh += 1; },
      selectChat: async () => { calls.selected.push(null); return true; },
      reloadChat: async () => { calls.reload += 1; return false; },
      rebuildChat: async () => { calls.rebuild += 1; return false; },
      currentChatId: () => ref.id,
      send: async () => (ref.id
        ? { delivered: true as const, sessionId: ref.id }
        : { delivered: false as const, sessionId: null, message: '消息未发送。' }),
      stop: async () => true,
      dismissChatError: () => {}, dismissListError: () => {},
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
    selected: selectedId ? selectedChat(selectedId) : null,
    historyReady: !failed,
    loadState: failed ? 'failed' : 'ready',
    chatError: failed ? '创建失败' : '',
    error: failed ? '创建失败' : '',
    banners: errorBanners({ listError: '', chatError: failed ? '创建失败' : '', loadState: failed ? 'failed' : 'ready' }),
    currentChatId: () => selectedRef.current,
    selectChat: async (target: { sessionId: string } | null) => {
      calls.selected += 1;
      if (target === null && createFailsOnce && attempts.current++ === 0) return choose(null, false);
      return choose(target?.sessionId ?? createId());
    },
    reloadChat: async () => {
      calls.reload += 1;
      return choose(createId());
    },
    send: async () => {
      const id = createId();
      choose(id);
      if (!id) return { delivered: false as const, sessionId: null, message: '消息未发送。' };
      return sendFails
        ? { delivered: false as const, sessionId: id, message: '消息未送达，已撤回。' }
        : { delivered: true as const, sessionId: id };
    },
  } as ChatFacade;

  return createElement(ChatShellView, { chat, onOpenSettings: () => {} });
}

describe('ChatShell real DOM recovery and panel interactions', () => {
  test('the header title follows the directory title for the open session', () => {
    const base = fakeChat({ initialId: 'chat-1' });
    const chat = {
      ...base.chat,
      // 副本会话信息不携带 Store 自动标题；目录轮询已经带回真实标题。
      chats: [summary('chat-1', '目录里的标题')],
    };
    render(createElement(ChatShellView, { chat: chat as ChatFacade, onOpenSettings: () => {} }));

    const header = document.querySelector('[data-slot="peri-chat-header"]') as HTMLElement;
    expect(within(header).getByText('目录里的标题')).toBeTruthy();
    expect(within(header).queryByText('chat-1')).toBeNull();
  });

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

describe('ChatShell blocked-state recovery', () => {
  test('executionBlocked shows a reason plus reload and close/rebuild actions', async () => {
    const base = fakeChat({ initialId: 'chat-a' });
    const chat = {
      ...base.chat,
      phase: 'blocked' as const,
      state: { ...emptyState('chat-a'), executionBlocked: true } as never,
      chatError: '',
      banners: {},
    };
    render(createElement(ChatShellView, { chat: chat as ChatFacade, onOpenSettings: () => {} }));

    const notice = document.querySelector('[data-slot="peri-chat-blocked"]')!;
    expect(notice.textContent).toContain('宿主执行被阻断');
    expect(within(notice as HTMLElement).getByText('重新加载')).toBeTruthy();
    expect(within(notice as HTMLElement).getByText('关闭并新建')).toBeTruthy();
    // 输入框给出真实阻断原因，而不是笼统的“等待会话同步”。
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).placeholder).toContain('执行被阻断');

    fireEvent.click(within(notice as HTMLElement).getByText('重新加载'));
    expect(base.calls.reload).toBe(1);
    await act(async () => {
      fireEvent.click(within(notice as HTMLElement).getByText('关闭并新建'));
      await Promise.resolve();
    });
    expect(base.calls.rebuild).toBe(1);
  });

  test('an unknown action result keeps its explanation and recovery on screen', () => {
    const base = fakeChat({ initialId: 'chat-a' });
    const message = '网络请求失败，动作执行状态未知。执行状态尚未确认，请重新加载会话后再继续。';
    const chat = {
      ...base.chat,
      phase: 'blocked' as const,
      state: emptyState('chat-a') as never,
      chatError: message,
      error: message,
      banners: errorBanners({ listError: '', chatError: message, loadState: 'ready', recovery: 'reload' }),
    };
    render(createElement(ChatShellView, { chat: chat as ChatFacade, onOpenSettings: () => {} }));

    const notice = document.querySelector('[data-slot="peri-chat-blocked"]')!;
    expect(notice.textContent).toContain('未确认');
    expect(within(notice as HTMLElement).getByText('重新加载')).toBeTruthy();
    // 阻断态不能被 dismiss 成「只剩一个禁用的输入框」。
    expect(screen.queryByLabelText('关闭错误提示')).toBeNull();
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).placeholder).toContain('未确认');
  });
});

describe('ChatShell binding-scoped draft slots', () => {
  test('a rebind to a different session never reuses the previous draft slot', () => {
    const base = fakeChat({ initialId: 'chat-a' });
    const view = render(createElement(ChatShellView, {
      chat: base.chat as ChatFacade, onOpenSettings: () => {},
    }));
    fireEvent.change(screen.getByLabelText('输入消息'), { target: { value: 'A 的草稿' } });
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('A 的草稿');

    // 副本换绑到新会话：展示身份跟着绑定走，草稿槽也随会话切换重置。
    const rebound = {
      ...base.chat,
      selected: selectedChat('chat-b'),
      currentChatId: () => 'chat-b',
    };
    view.rerender(createElement(ChatShellView, {
      chat: rebound as ChatFacade, onOpenSettings: () => {},
    }));
    expect((screen.getByLabelText('输入消息') as HTMLTextAreaElement).value).toBe('');
  });

  test('switching between existing sessions keeps each draft in its own slot', async () => {
    render(createElement(Harness, { initialId: 'chat-a', existingChats: ['chat-a', 'chat-b'] }));
    const textarea = () => screen.getByLabelText('输入消息') as HTMLTextAreaElement;

    fireEvent.change(textarea(), { target: { value: 'A 草稿' } });
    await act(async () => { fireEvent.click(screen.getByText('chat-b')); });
    expect(textarea().value).toBe('');

    fireEvent.change(textarea(), { target: { value: 'B 草稿' } });
    await act(async () => { fireEvent.click(screen.getByText('chat-a')); });
    expect(textarea().value).toBe('A 草稿');

    await act(async () => { fireEvent.click(screen.getByText('chat-b')); });
    expect(textarea().value).toBe('B 草稿');
  });
});

describe('ChatShell OpenUI action routing', () => {
  test('assistant Button action reuses send with compacted form values', async () => {
    stubShellLayout();
    const base = fakeChat({ initialId: 'chat-openui' });
    const sent: string[] = [];
    const program = [
      '```openui',
      'root = Stack([input, submit])',
      'input = TextInput("name", "姓名", "请输入")',
      'submit = Button("提交", @ToAssistant("提交表单"))',
      '```',
    ].join('\n');
    const state = openUIState(program);
    const chat = {
      ...base.chat,
      state,
      send: async (content: string) => {
        sent.push(content);
        return { delivered: true as const, sessionId: 'chat-openui' };
      },
    } as unknown as ChatFacade;
    render(createElement(ChatShellView, { chat, onOpenSettings: () => {} }));

    fireEvent.change(await screen.findByPlaceholderText('请输入'), { target: { value: 'Ada' } });
    fireEvent.click(screen.getByRole('button', { name: '提交' }));
    await waitFor(() => expect(sent).toHaveLength(1));
    expect(sent[0]).toBe('提交表单\n\n表单数据：{"name":"Ada"}');
  });

  test('open_url 只对 http(s) 调用 window.open', async () => {
    stubShellLayout();
    const open = spyOn(window, 'open').mockImplementation(() => null);
    const warn = spyOn(console, 'warn').mockImplementation(() => {});
    const base = fakeChat({ initialId: 'chat-open-url' });
    const program = [
      '```openui',
      'root = Stack([bad, good])',
      'bad = Button("坏", @OpenUrl("javascript:alert(1)"))',
      'good = Button("好", @OpenUrl("https://example.com"))',
      '```',
    ].join('\n');
    const chat = { ...base.chat, state: openUIState(program, 'chat-open-url') } as unknown as ChatFacade;
    render(createElement(ChatShellView, { chat, onOpenSettings: () => {} }));

    fireEvent.click(await screen.findByRole('button', { name: '坏' }));
    expect(open).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: '好' }));
    await waitFor(() => expect(open).toHaveBeenCalledTimes(1));
    expect(open).toHaveBeenCalledWith('https://example.com/', '_blank', 'noopener,noreferrer');
    open.mockRestore();
    warn.mockRestore();
  });
});

describe('ChatShell composer facts', () => {
  test('passes config, usage and history facts into the composer', () => {
    const base = fakeChat({ initialId: 'chat-facts' });
    const state = {
      ...emptyState('chat-facts'),
      entries: [
        {
          entryId: 'user-1', id: 'user-1', kind: 'message', turnId: 'turn-1', role: 'user',
          status: 'completed', createdAt: '2026-10-20T00:00:00Z', createdAtSource: 'observed',
          completedAt: '2026-10-20T00:00:01Z', error: null,
          blocks: [{ blockId: 'text:0', type: 'text', text: '旧问题' }],
        },
        {
          entryId: 'assistant-1', id: 'assistant-1', kind: 'message', turnId: 'turn-1', role: 'assistant',
          status: 'completed', createdAt: '2026-10-20T00:00:02Z', createdAtSource: 'observed',
          completedAt: '2026-10-20T00:00:03Z', error: null,
          tokenUsage: { totalTokens: 50, contextWindow: 200 },
          blocks: [{ blockId: 'text:1', type: 'text', text: '回答' }],
        },
      ],
      session: {
        configOptions: [{
          id: 'model', name: 'Model', type: 'select', category: 'model', currentValue: 'fast',
          options: [{ value: 'fast', name: 'Fast' }, { value: 'quality', name: 'Quality' }],
        }],
      },
    };
    const chat = { ...base.chat, state } as unknown as ChatFacade;
    render(createElement(ChatShellView, { chat, onOpenSettings: () => {} }));

    expect(screen.getByText('Fast')).toBeTruthy();
    expect(screen.getByRole('button', { name: /上下文用量 25%/ })).toBeTruthy();

    const textarea = screen.getByLabelText('输入消息') as HTMLTextAreaElement;
    textarea.setSelectionRange(0, 0);
    fireEvent.keyDown(textarea, { key: 'ArrowUp', code: 'ArrowUp', keyCode: 38 });
    expect(textarea.value).toBe('旧问题');
  });
});
