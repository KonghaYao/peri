// 左栏列表：工作区 + 实例 + 对话（原 panel.html 左栏 + ui.js
// renderInstances/renderChats）。ACP 会话列表已迁入 ChatHeader title 的
// tooltip（§6.3/§8.5），此处不再展示。对话操作（新建/新会话/取消/关闭）
// 收敛在「对话」标题行右侧的 icon 按钮组。

import { createSignal, For, Show } from 'solid-js';
import {
  cancelTurn,
  chatStatusSignal,
  chats,
  closeChat,
  createWorkspace,
  instances,
  isTerminal,
  newChat,
  newSession,
  removeWorkspace,
  selectChat,
  selectedCid,
  selectedWsId,
  setSelectedWsId,
  workspaces,
} from '../store';
import { Badge } from './Badge';

function shortId(id: string | null | undefined): string {
  if (!id) return '—';
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

// ── 工作区（独立于 chat 的上层概念，§6.3）──────────────────────────────

// 选中工作区后：对话列表按 workspace_id 过滤、ACP 会话按 cwd 过滤；
// 新建对话/进入历史会话继承该工作区 cwd。「全部」= 不过滤（兼容未归
// workspace 的旧数据）。
export function WorkspaceList() {
  const [name, setName] = createSignal('');
  const [cwd, setCwd] = createSignal('');
  const [expanded, setExpanded] = createSignal(false);

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    if (!cwd().trim()) return;
    createWorkspace(name().trim(), cwd().trim());
    setName('');
    setCwd('');
    setExpanded(false);
  };

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 text-sm font-semibold">
        工作区 <span class="font-normal text-slate-500">({workspaces().length})</span>
      </h2>
      <ul class="m-0 list-none p-0">
        <li
          onClick={() => setSelectedWsId(null)}
          class={`mb-1.5 cursor-pointer rounded px-1.5 py-1 text-sm hover:bg-slate-100 ${
            selectedWsId() === null ? 'bg-blue-50 ring-1 ring-blue-200' : ''
          }`}
        >
          全部（不过滤）
        </li>
        <For each={workspaces()}>
          {(w) => (
            <li
              onClick={() => setSelectedWsId(w.id)}
              class={`mb-1.5 cursor-pointer rounded px-1.5 py-1 hover:bg-slate-100 ${
                selectedWsId() === w.id ? 'bg-blue-50 ring-1 ring-blue-200' : ''
              }`}
            >
              <div class="flex items-center gap-1.5 text-sm">
                <span class="min-w-0 truncate">{w.name || shortId(w.id)}</span>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    removeWorkspace(w.id);
                  }}
                  class="ml-auto shrink-0 rounded px-1 text-xs text-slate-400 hover:bg-red-50 hover:text-red-600"
                  title="删除工作区定义（不影响已建对话）"
                >
                  删
                </button>
              </div>
              <div class="truncate text-xs text-slate-500" title={w.cwd}>
                {w.cwd}
              </div>
            </li>
          )}
        </For>
      </ul>
      <Show
        when={expanded()}
        fallback={
          <button
            onClick={() => setExpanded(true)}
            class="mt-1 w-full rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-100"
          >
            + 新建工作区
          </button>
        }
      >
        <form onSubmit={submit} class="mt-2 flex flex-col gap-1.5">
          <input
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
            placeholder="名称（留空 = 目录名）"
            class="w-full rounded border border-slate-300 px-1.5 py-1 text-xs"
          />
          <input
            value={cwd()}
            onInput={(e) => setCwd(e.currentTarget.value)}
            placeholder="本地目录（绝对路径，须已存在）"
            class="w-full rounded border border-slate-300 px-1.5 py-1 text-xs"
          />
          <div class="flex gap-1.5">
            <button
              type="submit"
              class="flex-1 rounded bg-blue-600 px-2 py-1 text-xs text-white hover:bg-blue-700 disabled:opacity-50"
              disabled={!cwd().trim()}
            >
              创建
            </button>
            <button
              type="button"
              onClick={() => setExpanded(false)}
              class="rounded border border-slate-300 px-2 py-1 text-xs text-slate-600 hover:bg-slate-100"
            >
              取消
            </button>
          </div>
        </form>
      </Show>
    </section>
  );
}

export function InstanceList() {  const list = () =>
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
  // 只显示非终态对话（accepting/active）；ended/closed/crashed 的历史对话
  // 不再展示（终态 chat 的 ACP 进程已退出，无操作价值）。
  // 按选中工作区过滤（§6.3）：workspace_id 匹配；「全部」= 不过滤。
  const active = () => {
    const wsId = selectedWsId();
    return chats().filter(
      (s) =>
        !['ended', 'closed', 'crashed'].includes(s.status || '') &&
        (wsId === null || s.workspaceId === wsId),
    );
  };
  const list = () =>
    active()
      .slice()
      .sort((a, b) => String(b.updatedAt || '').localeCompare(String(a.updatedAt || '')));

  // 新会话/取消/关闭针对当前选中对话；未选中或对话已终态（进程退出）
  // 时禁用。新建对话始终可用（创建后自动选中）。
  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);
  const opsDisabled = () => !selectedCid() || terminal();
  // icon 按钮样式（参照现有 icon 按钮：tooltip 刷新按钮/工作区删除按钮）。
  const opBtnClass =
    'flex h-5 w-5 items-center justify-center rounded border border-slate-300 text-xs leading-none text-slate-500 hover:bg-slate-100 hover:text-slate-700 disabled:opacity-45 disabled:hover:bg-transparent disabled:hover:text-slate-500';

  return (
    <section class="mb-3 rounded-lg border border-slate-300 bg-white p-3">
      <h2 class="mb-2 flex items-center gap-1.5 text-sm font-semibold">
        对话 <span class="font-normal text-slate-500">({active().length})</span>
        <span class="ml-auto flex shrink-0 items-center gap-1">
          <button
            onClick={newChat}
            title="新建对话"
            class={opBtnClass}
          >
            ＋
          </button>
          <button
            onClick={newSession}
            disabled={opsDisabled()}
            title="新会话（当前对话内创建）"
            class={opBtnClass}
          >
            ↻
          </button>
          <button
            onClick={cancelTurn}
            disabled={opsDisabled()}
            title="取消当前输出"
            class={opBtnClass}
          >
            ⏹
          </button>
          <button
            onClick={closeChat}
            disabled={opsDisabled()}
            title="关闭对话"
            class={opBtnClass}
          >
            ✕
          </button>
        </span>
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