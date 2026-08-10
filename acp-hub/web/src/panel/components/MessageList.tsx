// 消息区：权限条 + 消息气泡列表（自动吸底滚动）。
//
// 由 ChatView 拆出（右区三区之一）；气泡内 reasoning 在前、正文在后，
// loading 状态统一由 LoadingDots 组件承担。

import { createEffect, createSignal, For, Show } from 'solid-js';
import type { ChatEntry } from '../lib/yjs';
import { chatEntries, permissions, resolvePermission } from '../store';
import { Badge } from './Badge';

function shortId(id: string | null | undefined, n = 8): string {
  if (!id) return '—';
  return id.length > n ? `${id.slice(0, n)}…` : id;
}

// ── 权限条 ──────────────────────────────────────────────────────────────

function PermissionBar() {
  const p = () => permissions()[0]; // M3 雏形：逐条展示第一条，处理完移除

  return (
    <Show when={p()}>
      {(perm) => (
        <section class="mb-3 rounded-lg border border-amber-300 bg-amber-50 p-3">
          <div class="mb-2 flex items-baseline justify-between gap-2">
            <strong class="text-sm">{perm().title || '权限请求'}</strong>
            <span class="text-xs text-slate-500">
              {perm().description || ''}
              {perm().toolCallId ? ` · tool=${shortId(perm().toolCallId)}` : ''}
            </span>
          </div>
          <div class="flex gap-2">
            <button
              onClick={() => resolvePermission(perm().permissionId || '', 'allow')}
              class="rounded bg-emerald-600 px-3 py-1 text-sm text-white hover:opacity-90"
            >
              允许
            </button>
            <button
              onClick={() => resolvePermission(perm().permissionId || '', 'deny')}
              class="rounded bg-red-600 px-3 py-1 text-sm text-white hover:opacity-90"
            >
              拒绝
            </button>
          </div>
        </section>
      )}
    </Show>
  );
}

// ── loading 组件 ────────────────────────────────────────────────────────

// 三点脉冲动画：streaming 光标与空体骨架共用；通过 className 调整尺寸/颜色
function LoadingDots(props: { className?: string }) {
  return (
    <span
      class={`inline-flex items-center ${props.className ?? ''}`}
      aria-label="加载中"
    >
      <For each={[0, 1, 2]}>
        {(i) => (
          <span
            class="animate-pulse leading-none"
            style={{ 'animation-delay': `${i * 0.15}s` }}
          >
            ●
          </span>
        )}
      </For>
    </span>
  );
}

// ── 消息气泡 ────────────────────────────────────────────────────────────

function MessageBubble(props: { entry: ChatEntry }) {
  const e = () => props.entry;
  const align = () =>
    e().role === 'user' ? 'justify-end' : e().role === 'system' ? 'justify-center' : 'justify-start';
  const bubbleCls = () =>
    e().role === 'user'
      ? 'bg-blue-600 text-white'
      : e().role === 'system'
        ? 'bg-slate-200 text-slate-700'
        : 'bg-white border border-slate-300';

  return (
    <div class={`mb-2 flex ${align()}`}>
      <div class={`max-w-[85%] rounded-lg px-3 py-2 text-sm ${bubbleCls()}`}>
        <div class="mb-1 flex items-center gap-1.5 text-xs opacity-80">
          <span>{(e().role || '?') + ' · ' + e().createdAt}</span>
          <Badge status={e().status} />
        </div>

        <For each={e().reasoning}>
          {(r) => (
            <details class="mt-1 rounded bg-slate-100 px-2 py-1 text-xs">
              <summary class="cursor-pointer text-slate-500">思考过程</summary>
              <pre class="mt-1 mb-0 whitespace-pre-wrap font-mono text-xs">{r.text}</pre>
            </details>
          )}
        </For>

        <Show when={e().text}>
          <div class="whitespace-pre-wrap break-words">
            {e().text}
            {e().status === 'streaming' && <LoadingDots className="ml-0.5" />}
          </div>
        </Show>

        <For each={e().toolCalls}>
          {(tc) => (
            <div class="mt-1 text-xs">
              工具调用: {tc.name || tc.toolCallId || '?'} [{tc.status || '—'}]
            </div>
          )}
        </For>

        <For each={e().resources}>
          {(r) => (
            <div class="mt-1 text-xs">
              资源: {r.name || r.resourceId || '?'} ({r.mediaType || '—'})
            </div>
          )}
        </For>

        <Show when={e().error}>
          <div class="mt-1 text-xs text-red-600">
            {e().error!.code}
            {e().error!.message ? `: ${e().error!.message}` : ''}
          </div>
        </Show>

        {/* 空体（pending/streaming 骨架）→ 占位 loading */}
        <Show when={!e().text && !e().reasoning.length && !e().toolCalls.length && !e().error}>
          <LoadingDots className="text-slate-400" />
        </Show>
      </div>
    </div>
  );
}

// ── 消息滚动区 ──────────────────────────────────────────────────────────

export function MessageList() {
  const [stick, setStick] = createSignal(true);
  let areaRef: HTMLDivElement | undefined;

  // 自动吸底（用户上滚时暂停）
  createEffect(() => {
    const list = chatEntries();
    if (stick() && areaRef && list.length) {
      areaRef.scrollTop = areaRef.scrollHeight;
    }
  });

  return (
    <>
      <PermissionBar />
      <section
        ref={areaRef}
        onScroll={(e) => {
          const el = e.currentTarget;
          setStick(el.scrollHeight - el.scrollTop - el.clientHeight < 40);
        }}
        class="mb-3 h-[52vh] overflow-y-auto rounded-lg border border-slate-300 bg-slate-50 p-3"
      >
        <For each={chatEntries()}>
          {(e) => <MessageBubble entry={e} />}
        </For>
      </section>
    </>
  );
}
