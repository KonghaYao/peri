import { afterEach, describe, expect, test } from 'bun:test';
import { installDom } from '../../test/dom';

installDom();

const { cleanup, fireEvent, render, screen, act } = await import('@testing-library/react');
const { Timeline } = await import('./Timeline');
const { createElement } = await import('react');

afterEach(() => cleanup());

function bigState(chatId: string, count: number) {
  const entries = Array.from({ length: count }, (_, index) => ({
    entryId: `entry:${index}`, id: `entry:${index}`, kind: 'message',
    turnId: `turn:${Math.floor(index / 2)}`,
    role: index % 2 === 0 ? 'user' : 'assistant',
    status: 'completed', createdAt: '2026-10-20T00:00:00Z', createdAtSource: 'observed',
    completedAt: '2026-10-20T00:00:01Z', error: null,
    blocks: [{ blockId: 'text:0', type: 'text', text: `消息 ${index}` }],
  }));
  return {
    chat: { id: chatId, title: '大时间线', updatedAt: '2026-10-20T00:00:00Z' },
    running: false, executionBlocked: false, readiness: 'ready', revision: 1,
    entries, toolCalls: [], turns: [], activeTurn: { turnId: null, status: null },
    session: {}, tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
    pendingInteractions: [], inputQueue: null,
  };
}

function stubLayout(): void {
  class ImmediateResizeObserver {
    constructor(private readonly callback: (entries: unknown[], observer: unknown) => void) {}
    observe(element: Element): void {
      const rect = element.getBoundingClientRect();
      this.callback([{ target: element, contentRect: rect }], this);
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
      return new DOMRect(0, 0, 760, 90);
    }
    if (this.classList.contains('legend-list-content-container')) return new DOMRect(0, 0, 760, 2_000);
    return new DOMRect(0, 0, 800, 600);
  };
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) { return this.getBoundingClientRect().height; },
  });
  Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
    configurable: true,
    get(this: HTMLElement) { return this.getBoundingClientRect().height; },
  });
  Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
    configurable: true,
    get(this: HTMLElement) {
      if (this.classList.contains('legend-list-content-container')) return 2_000;
      // 时间线滚动容器在挂载前就要有可滚动高度，否则初始定位会被 clamp 到 0。
      if (this.getAttribute('data-slot') === 'peri-timeline') return 4_000;
      return 600;
    },
  });
  // happy-dom 没有实现 Element.scrollTo：补一个最小实现，让 LegendList 的初始化/回底真实更新 scrollTop。
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

function stubViewport(element: HTMLElement): void {
  Object.defineProperty(element, 'scrollHeight', { value: 4_000, configurable: true });
  Object.defineProperty(element, 'clientHeight', { value: 600, configurable: true });
  element.getBoundingClientRect = () => new DOMRect(0, 0, 800, 600);
}

describe('Timeline virtualization in a real DOM', () => {
  test('500+ derived rows do not mount every row at once', () => {
    stubLayout();
    const state = bigState('chat-big', 560);
    const { container } = render(createElement(Timeline, {
      state: state as never, loadState: 'ready', bottomInset: 0, scrollSignal: null,
    }));
    const mounted = container.querySelectorAll('[data-timeline-row-kind]');
    // 560 条 entry → 560 行；窗口化后只挂载视口附近的少量行。
    expect(mounted.length).toBeGreaterThan(0);
    expect(mounted.length).toBeLessThan(100);
    expect(mounted.length).toBeLessThan(560);
    expect(container.querySelector('[data-slot="peri-timeline"]')).not.toBeNull();
  });

  test('scrolling up shows the return-to-bottom control and clicking it pins again', async () => {
    stubLayout();
    const state = bigState('chat-scroll', 40);
    const { container } = render(createElement(Timeline, {
      state: state as never, loadState: 'ready', bottomInset: 0, scrollSignal: null,
    }));
    const scroller = container.querySelector('[data-slot="peri-timeline"]') as HTMLElement;
    stubViewport(scroller);
    // 等待首帧贴底完成，模拟用户随后主动上滚。
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    fireEvent.scroll(scroller, { target: { scrollTop: 0 } });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    const button = screen.getByText('回到底部');
    fireEvent.click(button);
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    // 回到底部后按钮消失、列表重新贴底。
    expect(screen.queryByText('回到底部')).toBeNull();
  });

  test('an up-scrolled position survives A → null → B → A and keeps the return control visible', async () => {    stubLayout();
    const chatA = bigState('chat-a', 40);
    const chatB = bigState('chat-b', 40);
    const props = (state: unknown) => ({
      state: state as never, loadState: 'ready' as const, bottomInset: 0, scrollSignal: null,
      emptyContent: createElement('p', null, '空会话'),
    });
    const view = render(createElement(Timeline, props(chatA)));
    const scrollerA = view.container.querySelector('[data-slot="peri-timeline"]') as HTMLElement;
    stubViewport(scrollerA);
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });

    // 用户在 A 上滚，离开贴底。
    fireEvent.scroll(scrollerA, { target: { scrollTop: 1000 } });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    expect(screen.getByText('回到底部')).toBeTruthy();

    // 切到空态（加载/新会话）再切 B，最后回到 A。
    await act(async () => { view.rerender(createElement(Timeline, props(null))); });
    await act(async () => { view.rerender(createElement(Timeline, props(chatB))); });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    await act(async () => { view.rerender(createElement(Timeline, props(chatA))); });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 120)); });

    const scrollerBack = view.container.querySelector('[data-slot="peri-timeline"]') as HTMLElement;
    stubViewport(scrollerBack);
    // 位置按锚点行 + 行内偏移恢复，而不是被旧 cleanup 覆盖成 atEnd。
    expect(scrollerBack.scrollTop).toBeGreaterThan(900);
    expect(scrollerBack.scrollTop).toBeLessThan(1_100);
    expect(screen.getByText('回到底部')).toBeTruthy();

    // 首屏挂载的是锚点附近的行（消息 10–14 在视口顶部），而不是末尾消息。
    const firstVisible = Array.from(view.container.querySelectorAll('[data-timeline-row-id]'))
      .map((element) => ({
        top: Number.parseFloat((element.parentElement as HTMLElement | null)?.style.top ?? 'NaN'),
        text: element.textContent ?? '',
      }))
      .filter((row) => Number.isFinite(row.top) && row.top >= scrollerBack.scrollTop - 1)
      .sort((left, right) => left.top - right.top)[0];
    expect(firstVisible?.text).toMatch(/消息 1[0-4]/);
  });

  test('a late send signal for another chat leaves the current up-scrolled timeline untouched', async () => {
    stubLayout();
    const chatB = bigState('chat-b', 40);
    const props = (scrollSignal: { token: number; chatId: string } | null) => ({
      state: chatB as never, loadState: 'ready' as const, bottomInset: 0, scrollSignal,
    });
    const view = render(createElement(Timeline, props(null)));
    const scroller = view.container.querySelector('[data-slot="peri-timeline"]') as HTMLElement;
    stubViewport(scroller);
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });

    // B 上滚到非贴底位置。
    fireEvent.scroll(scroller, { target: { scrollTop: 1_000 } });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    expect(screen.getByText('回到底部')).toBeTruthy();

    // A 的 prompt 迟到结算：信号目标不是当前会话 B，滚动位置与回底按钮都必须保持。
    await act(async () => {
      view.rerender(createElement(Timeline, props({ token: 1, chatId: 'chat-a' })));
      await new Promise((resolve) => setTimeout(resolve, 40));
    });
    expect(scroller.scrollTop).toBe(1_000);
    expect(screen.getByText('回到底部')).toBeTruthy();

    // 当前会话自己的新信号仍然有效。
    await act(async () => {
      view.rerender(createElement(Timeline, props({ token: 2, chatId: 'chat-b' })));
      await new Promise((resolve) => setTimeout(resolve, 40));
    });
    expect(screen.queryByText('回到底部')).toBeNull();
  });
});
