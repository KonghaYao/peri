// 左栏列表：工作区 + 实例 + 对话（原 panel.html 左栏 + ui.js
// renderInstances/renderChats）+ SidebarNav 组装导出。
//
// 低装饰化（ui.md 步骤 3）：去掉白卡片，改为 sidebar 内连续导航分组
// （§3.7 label + 列表），选中态浅灰圆角、hover 120ms（§3.8）；图标统一
// 内联 SVG（§3.6：20×20 viewBox、1.75px stroke、currentColor），不混用
// emoji/Unicode 图标。工作区/chat 的过滤、排序与全部 store action 零改动。
//
// ACP 会话列表已迁入 ChatHeader title 的 tooltip（§6.3/§8.5），此处不再
// 展示。对话操作（新建/新会话/取消/关闭）收敛在「对话」标题行右侧的
// icon 按钮组。
//
// SidebarNav 是左栏组装导出（§3.10 顺序：品牌行 → 新对话 → 工作区 →
// 实例 → 会话 → 底部连接条），由 AppShell 单行消费。

import { createSignal, For, Show } from 'solid-js';
import type { JSX } from 'solid-js';
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
import { badgeKind } from './Badge';
import { ConnectCard } from './ConnectCard';

function shortId(id: string | null | undefined): string {
  if (!id) return '—';
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

// ── 内联 SVG icon（§3.6）───────────────────────────────────────────────

type IconName =
  | 'compose'
  | 'folder'
  | 'plus'
  | 'refresh'
  | 'stop'
  | 'close'
  | 'chevron'
  | 'gear';

function iconPath(name: IconName): JSX.Element {
  switch (name) {
    case 'compose':
      // 新对话：message-square
      return <path d="M3.5 5A1.5 1.5 0 0 1 5 3.5h10A1.5 1.5 0 0 1 16.5 5v8a1.5 1.5 0 0 1-1.5 1.5H8.2l-4.7 3.8V5z" />;
    case 'folder':
      // 工作区：文件夹
      return (
        <path d="M2.5 6.5c0-.83.67-1.5 1.5-1.5h3.6c.4 0 .77.16 1.05.44l.9.9c.28.28.66.44 1.05.44H16c.83 0 1.5.67 1.5 1.5v5.7c0 .83-.67 1.5-1.5 1.5H4c-.83 0-1.5-.67-1.5-1.5v-7.5z" />
      );
    case 'plus':
      return <path d="M10 4.5v11M4.5 10h11" />;
    case 'refresh':
      // 新会话：rotate-cw（圆环开口朝右上 + 箭头）
      return (
        <>
          <path d="M16.5 7.5a7 7 0 1 0 1 5.5" />
          <path d="M17.5 3.5v4h-4" />
        </>
      );
    case 'stop':
      // 取消：圆角方块
      return <rect x="5.5" y="5.5" width="9" height="9" rx="1.5" />;
    case 'close':
      return <path d="M5.5 5.5l9 9M14.5 5.5l-9 9" />;
    case 'chevron':
      // 折叠指示：向下箭头（展开态由调用方 rotate-180）
      return <path d="M6.5 8.5l3.5 3.5 3.5-3.5" />;
    case 'gear':
      // 连接设置：sliders-vertical（两根滑杆 + 手柄）
      return (
        <>
          <path d="M6.5 3.5v4M6.5 12.5v4M13.5 3.5v8M13.5 15.5v1" />
          <path d="M3.5 6.5h6M10.5 12h6" />
        </>
      );
  }
}

// 尺寸不写死：CSS 同特异性按文件内顺序后者胜出，内部固定 h-5 w-5 会覆盖
// 调用处的 h-4 w-4（行内辅助操作 16px，§3.6），因此尺寸一律由调用方指定。
export function Icon(props: { name: IconName; class?: string }) {
  return (
    <svg
      viewBox="0 0 20 20"
      fill="none"
      stroke="currentColor"
      stroke-width={1.75}
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      class={`shrink-0 ${props.class ?? ''}`}
    >
      {iconPath(props.name)}
    </svg>
  );
}

// ── 共享导航项样式（§3.4：最小高 40px、8px 圆角、水平 padding 10px；
//    §3.8：hover 120ms；selected 浅灰底、主文字 500，不加蓝条）─────────

// 单行导航项（图标 + 文字同行）
const navItemCls =
  'flex min-h-10 cursor-pointer items-center gap-2 rounded-lg px-2.5 text-sm text-[var(--text-primary)] transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)]';

// 两行导航项（主行 + 副行；主行 500 由调用方控制）
const navItemRowCls =
  'group min-h-10 cursor-pointer rounded-lg px-2.5 py-1.5 text-sm text-[var(--text-primary)] transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)]';

const navItemSelectedCls = 'bg-[var(--selected)]';

// 危险操作（删除/关闭）：默认透明，父项 hover / :focus-within 才显示（§3.8）
const dangerBtnCls =
  'ml-auto flex h-5 w-5 shrink-0 items-center justify-center rounded text-[var(--text-faint)] opacity-0 transition-opacity duration-[120ms] ease-out group-hover:opacity-100 group-focus-within:opacity-100 hover:bg-[var(--danger-soft)] hover:text-[var(--danger)]';

// 表单输入（§3.5：1px strong 边框，focus 变 accent + 2px focus ring；
//    §3.8：36px 高、8px 圆角）。ring 用 outline 而非 box-shadow，与全局
//    :focus-visible 同值，鼠标点击聚焦时也显示、键盘聚焦不叠加双层环。
const inputCls =
  'h-9 w-full rounded-lg border border-[var(--border-strong)] bg-[var(--surface)] px-2.5 text-sm text-[var(--text-primary)] placeholder:text-[var(--text-faint)] focus:border-[var(--accent)] focus:outline-2 focus:outline-[var(--focus-ring)]';

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
    // max-h + overflow：工作区多时不把对话列表（flex-1 min-h-0）压到 0，
    // 保持 §3.7「对话列表独立滚动」；超出部分本组内滚动。
    <section class="ui-scrollbar max-h-[30vh] shrink-0 overflow-y-auto">
      <h2 class="pt-5 pb-2 text-xs font-medium text-[var(--text-muted)]">
        工作区 <span>({workspaces().length})</span>
      </h2>
      <ul class="m-0 list-none p-0">
        <li
          onClick={() => setSelectedWsId(null)}
          class={`${navItemCls} ${
            selectedWsId() === null ? `${navItemSelectedCls} font-medium` : ''
          }`}
        >
          全部（不过滤）
        </li>
        <For each={workspaces()}>
          {(w) => {
            const sel = () => selectedWsId() === w.id;
            return (
              <li
                onClick={() => setSelectedWsId(w.id)}
                class={`${navItemRowCls} ${sel() ? navItemSelectedCls : ''}`}
              >
                <div class="flex min-w-0 items-center gap-2">
                  <Icon name="folder" class="h-5 w-5 text-[var(--text-secondary)]" />
                  <span class={`min-w-0 flex-1 truncate ${sel() ? 'font-medium' : ''}`}>
                    {w.name || shortId(w.id)}
                  </span>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      removeWorkspace(w.id);
                    }}
                    class={dangerBtnCls}
                    title="删除工作区定义（不影响已建对话）"
                    aria-label="删除工作区"
                  >
                    <Icon name="close" class="h-4 w-4" />
                  </button>
                </div>
                <div class="truncate pl-7 text-xs leading-[17px] text-[var(--text-muted)]" title={w.cwd}>
                  {w.cwd}
                </div>
              </li>
            );
          }}
        </For>
      </ul>
      <Show
        when={expanded()}
        fallback={
          <button
            onClick={() => setExpanded(true)}
            class="flex min-h-10 w-full items-center gap-2 rounded-lg px-2.5 text-sm text-[var(--text-secondary)] transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)]"
          >
            <Icon name="plus" class="h-4 w-4" />
            新建工作区
          </button>
        }
      >
        <form onSubmit={submit} class="flex flex-col gap-1.5 py-2">
          <input
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
            placeholder="名称（留空 = 目录名）"
            class={inputCls}
          />
          <input
            value={cwd()}
            onInput={(e) => setCwd(e.currentTarget.value)}
            placeholder="本地目录（绝对路径，须已存在）"
            class={inputCls}
          />
          <div class="flex gap-1.5">
            <button
              type="submit"
              disabled={!cwd().trim()}
              class="h-9 flex-1 rounded-lg bg-[var(--btn-primary)] px-2.5 text-sm text-white transition-colors duration-[120ms] ease-out hover:bg-[var(--btn-primary-hover)] disabled:cursor-not-allowed disabled:opacity-45"
            >
              创建
            </button>
            <button
              type="button"
              onClick={() => setExpanded(false)}
              class="h-9 rounded-lg border border-[var(--border-strong)] px-2.5 text-sm text-[var(--text-secondary)] transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)]"
            >
              取消
            </button>
          </div>
        </form>
      </Show>
    </section>
  );
}

// ── 在线实例（可折叠分组）──────────────────────────────────────────────

export function InstanceList() {
  const [expanded, setExpanded] = createSignal(true);
  const list = () =>
    instances().slice().sort((a, b) => String(a.hostname).localeCompare(String(b.hostname)));

  return (
    // max-h + overflow：实例多时不把对话列表压到 0（同 WorkspaceList）。
    <section class="ui-scrollbar max-h-[30vh] shrink-0 overflow-y-auto">
      <button
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded()}
        class="flex w-full items-center gap-1.5 pt-5 pb-2 text-xs font-medium text-[var(--text-muted)] transition-colors duration-[120ms] ease-out hover:text-[var(--text-secondary)]"
      >
        实例 <span>({instances().length})</span>
        <Icon
          name="chevron"
          class={`ml-auto h-4 w-4 transition-transform duration-[120ms] ease-out ${
            expanded() ? 'rotate-180' : ''
          }`}
        />
      </button>
      <Show when={expanded()}>
        <ul class="m-0 list-none p-0">
          <For each={list()}>
            {(m) => (
              <li class="min-h-10 rounded-lg px-2.5 py-1.5 text-sm transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)]">
                <div class="flex items-center gap-2">
                  <span
                    class={`h-1.5 w-1.5 shrink-0 rounded-full ${
                      m.status === 'online' ? 'bg-[var(--success)]' : 'bg-[var(--text-faint)]'
                    }`}
                    aria-hidden="true"
                  />
                  <span class="min-w-0 flex-1 truncate">{m.hostname || m.id || '?'}</span>
                </div>
                <div class="truncate pl-3.5 text-xs leading-[17px] text-[var(--text-muted)]">
                  {m.status || '—'} · 对话{' '}
                  {m.chatCount === null || m.chatCount === undefined ? '—' : String(m.chatCount)} ·{' '}
                  {shortId(m.id)}
                </div>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </section>
  );
}

// ── 对话（左栏主要滚动列表）────────────────────────────────────────────

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
  // 32px 紧凑 icon button（§3.4）；触控场景（<768px drawer）命中区升到 40×40；
  // icon-only 必须有 aria-label（§3.6）。
  const opBtnCls =
    'flex h-10 w-10 shrink-0 items-center justify-center rounded-lg text-[var(--text-secondary)] transition-colors duration-[120ms] ease-out hover:bg-[var(--hover)] disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-transparent md:h-8 md:w-8';

  return (
    <section class="flex min-h-0 flex-1 flex-col">
      <h2 class="flex shrink-0 items-center gap-1.5 pt-5 pb-2 text-xs font-medium text-[var(--text-muted)]">
        对话 <span>({active().length})</span>
        <span class="ml-auto flex shrink-0 items-center gap-0.5">
          <button onClick={newChat} title="新建对话" aria-label="新建对话" class={opBtnCls}>
            <Icon name="plus" class="h-5 w-5" />
          </button>
          <button
            onClick={newSession}
            disabled={opsDisabled()}
            title="新会话（当前对话内创建）"
            aria-label="新会话（当前对话内创建）"
            class={opBtnCls}
          >
            <Icon name="refresh" class="h-5 w-5" />
          </button>
          <button
            onClick={cancelTurn}
            disabled={opsDisabled()}
            title="取消当前输出"
            aria-label="取消当前输出"
            class={opBtnCls}
          >
            <Icon name="stop" class="h-5 w-5" />
          </button>
          <button
            onClick={closeChat}
            disabled={opsDisabled()}
            title="关闭对话"
            aria-label="关闭对话"
            class={opBtnCls}
          >
            <Icon name="close" class="h-5 w-5" />
          </button>
        </span>
      </h2>
      <ul class="ui-scrollbar m-0 min-h-0 flex-1 list-none overflow-y-auto p-0 pb-4">
        <For each={list()}>
          {(s) => {
            const isSel = () => selectedCid() === s.id;
            const meta = () => [s.cwd, s.status, shortId(s.id)].filter(Boolean).join(' · ');
            return (
              <li
                onClick={() => selectChat(s.id)}
                class={`${navItemRowCls} ${isSel() ? navItemSelectedCls : ''}`}
              >
                <div class="flex min-w-0 items-center gap-2">
                  <span class={`min-w-0 flex-1 truncate ${isSel() ? 'font-medium' : ''}`}>
                    {s.title || shortId(s.id)}
                  </span>
                  {/* 蓝点：非终态活跃（warn 集与 Badge.tsx 同源，§3.8 streaming） */}
                  <Show when={badgeKind(s.status) === 'warn'}>
                    <span
                      class="h-1.5 w-1.5 shrink-0 rounded-full bg-[var(--accent)]"
                      aria-hidden="true"
                    />
                  </Show>
                </div>
                <div class="truncate pl-7 text-xs leading-[17px] text-[var(--text-muted)]">
                  {meta()}
                </div>
              </li>
            );
          }}
        </For>
      </ul>
    </section>
  );
}

// ── 左栏组装（§3.10 顺序：品牌 → 新对话 → 工作区 → 实例 → 会话 → 连接）─

export function SidebarNav() {
  return (
    // 不设 aria-label：外层 aside 已命名「导航」，内层 nav 再命名会形成
    // 两个同名 landmark（读屏器重复列出），未命名 nav 与已命名 aside 可区分。
    <nav class="flex h-full min-h-0 flex-col px-4">
      {/* 品牌行：64px，16/22/600，不使用大 logo（§3.7） */}
      <div class="flex h-16 shrink-0 items-center text-base font-semibold">acp-hub</div>
      {/* 「新对话」：ghost 导航项而非蓝色 CTA（§3.7） */}
      <button onClick={newChat} class={`${navItemCls} shrink-0`}>
        <Icon name="compose" class="h-5 w-5 text-[var(--text-secondary)]" />
        <span class="min-w-0 truncate">新对话</span>
      </button>
      <WorkspaceList />
      <InstanceList />
      <ChatList />
      {/* 底部连接区：与 sidebar 同底、顶部 divider，非悬浮卡片（§3.7） */}
      <ConnectCard />
    </nav>
  );
}
