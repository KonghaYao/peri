// 对话头部：固定高度 toolbar（ui.md §四.5 + §3.8）。
//
// 结构：左栏开关（<768px）｜标题 + workspace/cwd meta + 会话历史按钮
// （popover 挂载点）｜右栏开关（<1280px）。左右开关是纯 UI props
// （ChatHeaderProps，ui.md §四.5 定义的 seam，不涉及协议），中窄屏时
// 打开 AppShell 的 drawer；组件不读取任何全局布局状态。
//
// title 点击 → tooltip 展示当前对话的 ACP 会话列表（agent 磁盘历史，
// 原左栏 SessionList 迁入，§6.3/§8.5）：点击历史会话 = 当前对话内 load
// 切换（不新建对话/进程）；「当前」会话（boundChatId === selectedCid）
// 标徽标且点击无操作。tooltip 交互：点击 title 或历史按钮切换开/关，
// 点击 root（标题块 + meta + 历史按钮 + popover）之外或 Esc 关闭。
// 会话数据来自 store 按对话隔离缓存（currentSessions）。
// 对话操作（新建/新会话/取消/关闭）已收敛到左侧对话列表区。

import { createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import {
  chatHead,
  chatStatusSignal,
  chats,
  currentSessions,
  isTerminal,
  openAcpSession,
  refreshSessions,
  selectedCid,
  workspaces,
} from '../store';
import { Badge } from './Badge';

export type ChatHeaderProps = {
  onOpenNavigation?: () => void;
  onOpenStatus?: () => void;
};

// ── 内联图标（§3.6：20×20 viewBox、1.75px stroke、round cap/join、
//    currentColor；不引入 icon 依赖）──────────────────────────────────────

function svgProps(className?: string) {
  return {
    viewBox: '0 0 20 20',
    fill: 'none',
    stroke: 'currentColor',
    'stroke-width': 1.75,
    'stroke-linecap': 'round' as const,
    'stroke-linejoin': 'round' as const,
    class: `h-5 w-5 ${className ?? ''}`,
    'aria-hidden': true,
  };
}

// 左栏开关：面板线框（左侧宽面板 + 右侧细边）。
function IconPanelLeft() {
  return (
    <svg {...svgProps()}>
      <rect x="2.5" y="3" width="15" height="14" rx="2" />
      <path d="M8 3v14" />
    </svg>
  );
}

// 右栏开关：面板线框（右侧宽面板 + 左侧细边）。
function IconPanelRight() {
  return (
    <svg {...svgProps()}>
      <rect x="2.5" y="3" width="15" height="14" rx="2" />
      <path d="M12 3v14" />
    </svg>
  );
}

// 会话历史：时钟回环。
function IconHistory() {
  return (
    <svg {...svgProps()}>
      <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
      <path d="M3 3v5h5" />
      <path d="M12 7v5l4 2" />
    </svg>
  );
}

// 刷新：旋转箭头；busy 时由外部加 animate-spin。
function IconRefresh(props: { className?: string }) {
  return (
    <svg {...svgProps(props.className)}>
      <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" />
      <path d="M21 3v5h-5" />
      <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" />
      <path d="M8 16H3v5" />
    </svg>
  );
}

// ── 工具函数 ─────────────────────────────────────────────────────────────

function shortId(id: string | null | undefined, n = 8): string {
  if (!id) return '—';
  return id.length > n ? `${id.slice(0, n)}…` : id;
}

// ACP 会话 id 是 UUID v7：前 12 位 = 毫秒时间戳，同一批（秒级内）创建的
// 会话前 8 位几乎必然相同（如 019fe6ee-* 一批）——短 id 必须带**尾部随机
// 段**（v7 区分度在尾部）才有辨识度，否则视觉上像同一会话重复出现。
function sessionShortId(id: string | null | undefined): string {
  if (!id) return '—';
  if (id.length <= 12) return id;
  return `${id.slice(0, 8)}…${id.slice(-4)}`;
}

// RFC3339 → 本地 HH:MM:SS（空串/非法 → '—'）。
function shortTime(s: string | null): string {
  if (!s || s === '—') return '—';
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleTimeString();
}

// ── ChatHeader ───────────────────────────────────────────────────────────

export function ChatHeader(props: ChatHeaderProps) {
  const ctrl = () => chatHead()?.chat;
  const activeTurn = () => chatHead()?.activeTurn;
  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);

  // ── ACP 会话 tooltip：点击 title/历史按钮切换开/关；点击 root（标题块
  //    + meta + 历史按钮 + popover）之外的任意处或按 Esc 关闭（document
  //    监听 + contains 判定）。
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  let rootRef: HTMLDivElement | undefined;

  const sessions = () => currentSessions();

  const refresh = () => {
    setBusy(true);
    refreshSessions();
    setTimeout(() => setBusy(false), 300);
  };

  onMount(() => {
    const onDocClick = (e: MouseEvent) => {
      if (!rootRef?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('click', onDocClick);
    document.addEventListener('keydown', onKey);
    onCleanup(() => {
      document.removeEventListener('click', onDocClick);
      document.removeEventListener('keydown', onKey);
    });
  });

  const title = () => {
    if (!selectedCid()) return '未选择对话';
    const t = ctrl()?.title;
    if (t) return t;
    const id = ctrl()?.chatId || selectedCid()!;
    return `对话 ${shortId(id)}${terminal() ? '（已结束）' : ''}`;
  };

  // workspace/cwd 元数据（§四.5）：workspace 名与对话 cwd 前置，其后保留
  // 原有 chatId · status · turn 信息。纯读 store（chats/workspaces 只读），
  // 不缓存派生。
  const meta = () => {
    if (!selectedCid()) return '点击左侧对话列表，或新建对话';
    const chat = chats().find((c) => c.id === selectedCid());
    const ws = chat?.workspaceId
      ? workspaces().find((w) => w.id === chat.workspaceId)
      : undefined;
    const parts: string[] = [];
    if (ws) parts.push(ws.name);
    if (chat?.cwd) parts.push(chat.cwd);
    const c = ctrl();
    if (c) {
      parts.push(c.chatId, c.status || '—');
      if (activeTurn()?.turnStatus) parts.push(`turn=${activeTurn()!.turnStatus}`);
    } else {
      parts.push(selectedCid()!);
      if (terminal()) parts.push(chatStatusSignal()[selectedCid()!] || '');
    }
    return parts.join(' · ');
  };

  return (
    <section class="flex h-16 max-md:h-14 shrink-0 items-center gap-3 border-b border-[var(--border-subtle)] bg-white px-5">
      {/* 左栏开关：仅 <768px（drawer 入口） */}
      <button
        onClick={() => props.onOpenNavigation?.()}
        aria-label="打开导航"
        title="打开导航"
        class="hidden h-8 w-8 shrink-0 items-center justify-center rounded-md text-[var(--text-secondary)] transition-colors duration-150 hover:bg-[var(--hover)] hover:text-[var(--text-primary)] max-md:inline-flex"
      >
        <IconPanelLeft />
      </button>

      {/* 标题块 + meta + 历史按钮 + popover：rootRef 覆盖整体，供外部点击判定 */}
      <div ref={rootRef} class="relative flex min-w-0 flex-1 items-center gap-2">
        <h2
          onClick={(e) => {
            // tooltip 内部点击（列表/刷新）不触发切换，只由 title 本体切换。
            if (e.target !== e.currentTarget) return;
            setOpen((v) => !v);
          }}
          class="min-w-0 cursor-pointer truncate text-base font-semibold text-[var(--text-primary)]"
          title="ACP 会话列表（点击切换）"
        >
          {title()}
        </h2>
        <p class="min-w-0 truncate text-xs text-[var(--text-muted)]">{meta()}</p>
        <button
          onClick={() => setOpen((v) => !v)}
          aria-label="ACP 会话历史"
          title="ACP 会话历史"
          class="ml-auto inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-[var(--text-secondary)] transition-colors duration-150 hover:bg-[var(--hover)] hover:text-[var(--text-primary)]"
        >
          <IconHistory />
        </button>
        <Show when={open()}>
          {/* popover 相对 rootRef（flex-1 块）absolute left-0 定位；窄屏
              rootRef 左边缘 = 20px（px-5）+ 32px（左栏开关）+ 12px（gap-3）
              = 64px，宽 336px 的右边缘在 390px 视口会溢出 10px（§3.13
              验收 9）。max-w 按 100vw-64px 收紧，保证右边缘不超过视口。 */}
          <div class="absolute top-full left-0 z-20 mt-2 w-[336px] max-w-[calc(100vw-64px)] rounded-xl border border-[var(--border-subtle)] bg-white p-3 shadow-[var(--shadow-popover)]">
            <div class="mb-1 flex items-center gap-1.5 text-xs font-semibold text-[var(--text-primary)]">
              ACP 会话{' '}
              <span class="font-normal text-[var(--text-muted)]">
                ({sessions().length})
              </span>
              <Show when={selectedCid() !== null}>
                <button
                  onClick={refresh}
                  disabled={busy()}
                  aria-label="刷新会话列表"
                  title="向 agent 侧重新查询（10s 自动刷新）"
                  class="ml-auto flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--hover)] hover:text-[var(--text-primary)] disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <IconRefresh className={busy() ? 'animate-spin' : undefined} />
                </button>
              </Show>
            </div>
            <Show
              when={selectedCid() !== null}
              fallback={<p class="text-xs text-[var(--text-muted)]">未选择对话（切换对话后按需查询）</p>}
            >
              <Show
                when={sessions().length > 0}
                fallback={<p class="text-xs text-[var(--text-muted)]">暂无 ACP 会话（10s 自动刷新）</p>}
              >
                <ul class="m-0 max-h-[360px] list-none overflow-y-auto p-0">
                  <For each={sessions()}>
                    {(s) => {
                      // §8.5 会话切换语义：会话是**进程内实体**——列表属于
                      // 当前对话（进程），boundChatId 标注该对话的**当前活跃
                      // 会话**（标「当前」，点击无操作）；其余为历史会话，
                      // 点击 = 在当前对话内 load 切换（不新建对话/进程）。
                      const isCurrent = s.boundChatId === selectedCid();
                      const open = () => {
                        if (isCurrent) return;
                        openAcpSession(s.sessionId, s.title || undefined);
                        setOpen(false);
                      };
                      return (
                        <li
                          onClick={open}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter' || e.key === ' ') {
                              e.preventDefault();
                              open();
                            }
                          }}
                          // 键盘可达（§3.13 验收 10）：历史会话可聚焦并以
                          // Enter/Space 触发切换；「当前」会话点击无操作，
                          // 不进入 tab 序，避免误导键盘用户。focus 环由
                          // styles.css 全局 :focus-visible 提供。
                          role={isCurrent ? undefined : 'button'}
                          tabIndex={isCurrent ? undefined : 0}
                          class={`mb-1 cursor-pointer rounded-lg px-2 py-1.5 ${
                            isCurrent
                              ? 'bg-[var(--selected)] hover:bg-[var(--selected)]'
                              : 'hover:bg-[var(--hover)]'
                          }`}
                          title={isCurrent ? '当前会话' : '在当前对话内切换到此会话（不新建对话）'}
                        >
                          <div class="flex items-center gap-1.5 text-sm">
                            <Show
                              when={s.title}
                              fallback={<span class="italic text-[var(--text-muted)]">（无标题）</span>}
                            >
                              <span class="min-w-0 truncate" title={s.title || ''}>
                                {s.title}
                              </span>
                            </Show>
                            <Show when={s.status}>
                              <Badge status={s.status} />
                            </Show>
                            <Show when={isCurrent}>
                              <span class="ml-auto shrink-0 rounded bg-[var(--hover)] px-1 py-0.5 text-[10px] text-[var(--text-secondary)]">
                                当前
                              </span>
                            </Show>
                          </div>
                          <div class="text-xs text-[var(--text-muted)]">
                            {shortTime(s.updatedAt)} · {sessionShortId(s.sessionId)}
                            {s.cwd ? ` · ${s.cwd}` : ''}
                          </div>
                        </li>
                      );
                    }}
                  </For>
                </ul>
              </Show>
            </Show>
          </div>
        </Show>
      </div>

      {/* 右栏开关：仅 <1280px（drawer 入口） */}
      <button
        onClick={() => props.onOpenStatus?.()}
        aria-label="打开状态栏"
        title="打开状态栏"
        class="hidden h-8 w-8 shrink-0 items-center justify-center rounded-md text-[var(--text-secondary)] transition-colors duration-150 hover:bg-[var(--hover)] hover:text-[var(--text-primary)] max-xl:inline-flex"
      >
        <IconPanelRight />
      </button>
    </section>
  );
}
