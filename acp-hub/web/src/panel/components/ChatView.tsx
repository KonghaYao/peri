// 右区：对话头部 + 工具栏 + 权限条 + 消息气泡 + 输入框。
//
// 对应原 panel.html 右区 + ui.js（setCurrentChat / renderChatHead /
// renderPermissions / renderChat / 输入区），状态由 store 信号派生。

import { createEffect, createSignal, For, Show } from 'solid-js';
import type { ChatEntry } from '../lib/yjs';
import {
  cancelTurn,
  chatEntries,
  chatHead,
  chatStatusSignal,
  closeChat,
  isTerminal,
  newChat,
  permissions,
  resolvePermission,
  selectedCid,
  sendMessage,
} from '../store';
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

        <Show when={e().text}>
          <div class="whitespace-pre-wrap break-words">
            {e().text}
            {e().status === 'streaming' && (
              <span class="ml-0.5 inline-block animate-pulse">▍</span>
            )}
          </div>
        </Show>

        <For each={e().reasoning}>
          {(r) => (
            <details class="mt-1 rounded bg-slate-100 px-2 py-1 text-xs">
              <summary class="cursor-pointer text-slate-500">reasoning（折叠）</summary>
              <pre class="mt-1 mb-0 whitespace-pre-wrap font-mono text-xs">{r.text}</pre>
            </details>
          )}
        </For>

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

        {/* 空体（pending/streaming 骨架）→ 占位光标 */}
        <Show when={!e().text && !e().reasoning.length && !e().toolCalls.length && !e().error}>
          <div class="animate-pulse">…</div>
        </Show>
      </div>
    </div>
  );
}

// ── 对话主区 ────────────────────────────────────────────────────────────

export function ChatView() {
  const ctrl = () => chatHead()?.chat;
  const activeTurn = () => chatHead()?.activeTurn;
  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);

  const title = () => {
    if (!selectedCid()) return '未选择对话';
    const t = ctrl()?.title;
    if (t) return t;
    const id = ctrl()?.chatId || selectedCid()!;
    return `对话 ${shortId(id)}${terminal() ? '（已结束）' : ''}`;
  };

  const meta = () => {
    if (!selectedCid()) return '点击左侧对话列表，或新建对话';
    const c = ctrl();
    if (c) {
      const parts = [c.chatId, c.status || '—'];
      if (activeTurn()?.turnStatus) parts.push(`turn=${activeTurn()!.turnStatus}`);
      return parts.join(' · ');
    }
    return `${selectedCid()}${terminal() ? ` · ${chatStatusSignal()[selectedCid()!] || ''}` : ''}`;
  };

  const inputDisabled = () => !selectedCid() || terminal();
  const inputPlaceholder = () =>
    !selectedCid()
      ? '输入消息，Enter 发送（需先选中对话）'
      : terminal()
        ? '对话已结束（历史只读）'
        : '输入消息，Enter 发送（需先选中对话）';

  const [msg, setMsg] = createSignal('');
  const [stick, setStick] = createSignal(true);
  let areaRef: HTMLDivElement | undefined;

  // 自动吸底（用户上滚时暂停）
  createEffect(() => {
    const list = chatEntries();
    if (stick() && areaRef && list.length) {
      areaRef.scrollTop = areaRef.scrollHeight;
    }
  });

  function submit() {
    const text = msg().trim();
    if (!text) return;
    setMsg('');
    sendMessage(text);
  }

  return (
    <>
      <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
        <div class="flex items-baseline justify-between gap-2">
          <div class="min-w-0">
            <h2 class="mt-0 mb-0.5 flex items-center gap-2 text-sm font-semibold">{title()}</h2>
            <p class="mt-0.5 mb-0 text-xs text-slate-500">{meta()}</p>
          </div>
          <div class="flex shrink-0 gap-2">
            <button
              onClick={newChat}
              class="rounded bg-blue-600 px-2.5 py-1 text-xs text-white hover:opacity-90"
            >
              新建对话
            </button>
            <button
              onClick={cancelTurn}
              disabled={!selectedCid() || terminal()}
              class="rounded bg-slate-500 px-2.5 py-1 text-xs text-white hover:opacity-90 disabled:opacity-45"
            >
              取消
            </button>
            <button
              onClick={closeChat}
              disabled={!selectedCid() || terminal()}
              class="rounded bg-slate-500 px-2.5 py-1 text-xs text-white hover:opacity-90 disabled:opacity-45"
            >
              关闭
            </button>
          </div>
        </div>
      </section>

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

      <section class="rounded-lg border border-slate-300 bg-white p-3">
        <div class="flex gap-2">
          <input
            type="text"
            value={msg()}
            onInput={(e) => setMsg(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
            placeholder={inputPlaceholder()}
            disabled={inputDisabled()}
            spellcheck={false}
            class="min-w-0 flex-1 rounded border border-slate-300 px-2 py-1 text-sm disabled:bg-slate-100 disabled:text-slate-400"
          />
          <button
            onClick={submit}
            disabled={inputDisabled()}
            class="rounded bg-blue-600 px-3 py-1 text-sm text-white hover:opacity-90 disabled:opacity-45"
          >
            发送
          </button>
        </div>
      </section>
    </>
  );
}
