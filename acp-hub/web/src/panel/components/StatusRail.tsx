// 状态 rail：连接状态机 / 心跳 / registry / 订阅 / 最近 ack / 最近错误。
//
// ui.md §四.8 状态工作区：顶部「状态/活动」tabs（文字 + 2px bottom
// indicator，§3.7），状态摘要 / 最近 ack / 最近错误各 section 独立折叠，
// 空状态见 §3.9。背景 / 边框 / 独立滚动由 AppShell 的 StatusSidebar
// 提供（rail-bg + overflow-y-auto），本组件只负责内容与分组，不重复套
// 滚动容器，也不改变任何状态含义（数据读取与 store 原样）。

import { createSignal, For, Show, type JSX } from 'solid-js';
import {
  ackLog,
  connState,
  errorLog,
  globalStatus,
  heartbeatCount,
  subscribedDocs,
} from '../store';
import { Badge } from './Badge';

type RailTab = 'status' | 'activity';

// 折叠分组头（§3.7：状态分组以 divider 分隔，不用白色卡片；label 12px/
// 500/muted；16px chevron，§3.6）。折叠状态为组件本地 signal，不进 store。
function RailSection(props: { title: string; children: JSX.Element }) {
  const [open, setOpen] = createSignal(true);
  return (
    <section class="border-t border-[var(--divider)]">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open()}
        class="flex w-full cursor-pointer items-center gap-1.5 px-4 py-2.5 text-left text-xs font-medium text-[var(--text-muted)] transition-colors duration-[120ms] ease-out hover:text-[var(--text-primary)]"
      >
        <svg
          viewBox="0 0 20 20"
          fill="none"
          stroke="currentColor"
          stroke-width={1.75}
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
          class={`h-4 w-4 shrink-0 transition-transform duration-[120ms] ease-out ${open() ? 'rotate-180' : ''}`}
        >
          <path d="M6.5 8.5l3.5 3.5 3.5-3.5" />
        </svg>
        {props.title}
      </button>
      <Show when={open()}>
        <div class="px-4 pb-4">{props.children}</div>
      </Show>
    </section>
  );
}

export function StatusRail() {
  const [tab, setTab] = createSignal<RailTab>('status');

  const tabs: { id: RailTab; label: string }[] = [
    { id: 'status', label: '状态' },
    { id: 'activity', label: '活动' },
  ];

  // roving tabindex + 方向键（WCAG tabs 模式）：切换选中并把焦点随动到
  // 新 tab，避免焦点环与 2px indicator 脱节；Tab 聚焦当前 tab。
  const onTabKey = (e: KeyboardEvent) => {
    const i = tabs.findIndex((t) => t.id === tab());
    let next: RailTab | null = null;
    if (e.key === 'ArrowRight') {
      e.preventDefault();
      next = tabs[(i + 1) % tabs.length].id;
    } else if (e.key === 'ArrowLeft') {
      e.preventDefault();
      next = tabs[(i - 1 + tabs.length) % tabs.length].id;
    }
    if (next !== null) {
      setTab(next);
      document.getElementById(`rail-tab-${next}`)?.focus();
    }
  };

  return (
    <>
      <div role="tablist" aria-label="状态与活动" class="flex h-16 max-md:h-14 shrink-0 items-end gap-5 px-4">
        <For each={tabs}>
          {(t) => (
            <button
              type="button"
              role="tab"
              id={`rail-tab-${t.id}`}
              aria-selected={tab() === t.id}
              aria-controls={`rail-panel-${t.id}`}
              tabIndex={tab() === t.id ? 0 : -1}
              onClick={() => setTab(t.id)}
              onKeyDown={onTabKey}
              class={`border-b-2 pb-3 text-sm font-medium transition-colors duration-[120ms] ease-out ${
                tab() === t.id
                  ? 'border-[var(--text-primary)] text-[var(--text-primary)]'
                  : 'border-transparent text-[var(--text-secondary)] hover:text-[var(--text-primary)]'
              }`}
            >
              {t.label}
            </button>
          )}
        </For>
      </div>

      {/* 状态 tab：状态摘要（连接健康态）+ 最近错误 */}
      <div
        role="tabpanel"
        id="rail-panel-status"
        aria-labelledby="rail-tab-status"
        class={tab() === 'status' ? '' : 'hidden'}
      >
        <RailSection title="状态摘要">
          <dl class="m-0 space-y-2 text-sm">
            {/* 徽标显示完整 text（信息原样），着色按 store 的 kind
                （''/ok/warn/err），Badge 的 kind prop 为显式着色。 */}
            <div class="flex items-center justify-between gap-2">
              <dt class="text-[var(--text-secondary)]">连接</dt>
              <dd class="m-0 shrink-0">
                <Badge status={connState().text} kind={connState().kind} />
              </dd>
            </div>
            <div class="flex items-center justify-between gap-2">
              <dt class="text-[var(--text-secondary)]">registry</dt>
              <dd class="m-0 shrink-0">
                <Badge status={globalStatus() || '—'} />
              </dd>
            </div>
            <div class="flex items-center justify-between gap-2">
              <dt class="text-[var(--text-secondary)]">心跳</dt>
              <dd class="m-0 shrink-0 font-mono text-xs">{heartbeatCount()}</dd>
            </div>
            <div class="flex items-center justify-between gap-2">
              <dt class="text-[var(--text-secondary)]">订阅</dt>
              <dd class="m-0 min-w-0 text-right font-mono text-xs break-all">
                {subscribedDocs()}
              </dd>
            </div>
          </dl>
        </RailSection>

        <RailSection title="最近错误">
          <Show
            when={errorLog().length > 0}
            fallback={<p class="py-1 text-xs text-[var(--text-muted)]">无最近错误</p>}
          >
            <ul class="m-0 list-none p-0">
              <For each={errorLog()}>
                {(line) => (
                  <li class="py-2 font-mono text-xs leading-5 break-all text-[var(--danger)]">
                    {line}
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </RailSection>
      </div>

      {/* 活动 tab：最近 ack（命令确认活动流） */}
      <div
        role="tabpanel"
        id="rail-panel-activity"
        aria-labelledby="rail-tab-activity"
        class={tab() === 'activity' ? '' : 'hidden'}
      >
        <RailSection title="最近 ack">
          <Show
            when={ackLog().length > 0}
            fallback={
              <p class="flex items-center gap-1.5 py-1 text-xs text-[var(--text-muted)]">
                <svg
                  viewBox="0 0 20 20"
                  fill="none"
                  stroke="currentColor"
                  stroke-width={1.75}
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                  class="h-4 w-4 shrink-0"
                >
                  <path d="M3 12h3l2-6 4 12 2-6h3" />
                </svg>
                暂无活动
              </p>
            }
          >
            <ul class="m-0 list-none p-0">
              <For each={ackLog()}>
                {(line) => (
                  <li class="py-2 font-mono text-xs leading-5 break-all text-[var(--text-secondary)]">
                    {line}
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </RailSection>
      </div>
    </>
  );
}
