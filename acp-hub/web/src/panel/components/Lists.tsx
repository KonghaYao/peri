// 左栏列表：实例 + 对话（原 panel.html 左栏 + ui.js renderInstances/renderChats）。

import { For } from 'solid-js';
import { chats, instances, selectChat, selectedCid } from '../store';
import { Badge } from './Badge';

function shortId(id: string | null | undefined): string {
  if (!id) return '—';
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

export function InstanceList() {
  const list = () =>
    instances().slice().sort((a, b) => String(a.hostname).localeCompare(String(b.hostname)));

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 text-sm font-semibold">
        实例 <span class="font-normal text-slate-500">({instances().length})</span>
      </h2>
      <ul class="m-0 list-none p-0">
        <For each={list()}>
          {(m) => (
            <li class="mb-1.5 rounded px-1.5 py-1 hover:bg-slate-100">
              <div class="flex items-center gap-1.5 text-sm">
                <span class="min-w-0 truncate">{m.hostname || m.id || '?'}</span>
                <Badge status={m.status} />
              </div>
              <div class="text-xs text-slate-500">
                对话 {m.chatCount === null || m.chatCount === undefined ? '—' : String(m.chatCount)} ·{' '}
                {shortId(m.id)}
              </div>
            </li>
          )}
        </For>
      </ul>
    </section>
  );
}

export function ChatList() {
  const list = () =>
    chats()
      .slice()
      .sort((a, b) => String(b.updatedAt || '').localeCompare(String(a.updatedAt || '')));

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 text-sm font-semibold">
        对话 <span class="font-normal text-slate-500">({chats().length})</span>
      </h2>
      <ul class="m-0 list-none p-0">
        <For each={list()}>
          {(s) => {
            const isSel = () => selectedCid() === s.id;
            return (
              <li
                onClick={() => selectChat(s.id)}
                class={`mb-1.5 cursor-pointer rounded px-1.5 py-1 hover:bg-slate-100 ${
                  isSel() ? 'bg-blue-50 ring-1 ring-blue-200' : ''
                }`}
              >
                <div class="flex items-center gap-1.5 text-sm">
                  <span class="min-w-0 truncate">{s.title || shortId(s.id)}</span>
                  <Badge status={s.status} />
                </div>
                <div class="text-xs text-slate-500">{shortTime(s.updatedAt)}</div>
              </li>
            );
          }}
        </For>
      </ul>
    </section>
  );
}

// RFC3339 → 本地 HH:MM:SS（空串/非法 → '—'）。
function shortTime(s: string | null): string {
  if (!s || s === '—') return '—';
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleTimeString();
}