// 应用壳：固定视口三栏 + 响应式 drawer（ui.md 阶段 1）。
//
// 纯布局组件：不导入 store / yjs / ws，drawer 状态（leftPanelOpen /
// rightPanelOpen）只属于本组件，不进全局 store。
//
// 断点（Tailwind 默认 md=768 / xl=1280，即 ui.md 三档，无新增 config）：
//   ≥1280px：三栏 grid（var(--sidebar-w) / fluid / var(--rail-w)），左右 relative
//   768–1279px：左栏 relative 常驻；右栏收为 overlay drawer（xl 起恢复第三列）
//   <768px：左右栏均为 overlay drawer（宽度 min(88vw, 320px)）
//
// drawer 用 CSS transform 切换且始终挂载（不卸载重建），切换面板不会重建
// ChatView / 聊天状态；fixed 遮罩拦截窄屏触摸滚动穿透，无需 JS 锁滚动。
// 遮罩只在该栏实际处于 drawer 覆盖形态时渲染（见底部 Show 条件），
// 常驻 grid 列不被遮罩盖住。
// Escape 只关 drawer，与 ChatHeader 的 Escape（关 ACP session tooltip）
// 各自独立监听、互不干扰。
// 关闭的 drawer（drawer 形态且未打开）加 inert：移出 tab 序与辅助技术树，
// 避免键盘用户聚焦到视口外的 SidebarNav/StatusRail；断点以上为 grid 常驻
// 列时豁免。drawer 覆盖形态打开时焦点移入 drawer 内首个可聚焦元素（本
// 实现无独立关闭按钮，以首元素代替 §3.10 的关闭按钮语义），关闭后焦点
// 返还触发按钮（见 AppShell 焦点管理 effect）。

import { createEffect, createSignal, onCleanup, onMount, Show } from 'solid-js';
import { ChatView } from './ChatView';
import { SidebarNav } from './Lists';
import { StatusRail } from './StatusRail';

// drawer 滑入动画（§3.11 缓动）。Tailwind v4 的 translate-x-* 使用 CSS
// `translate` 属性（非 transform），transition-transform 内置覆盖
// transform, translate, scale, rotate；reduced-motion 由 styles.css
// 全局规则归零。
const drawerCls =
  'transition-transform duration-[180ms] ease-[cubic-bezier(.2,.8,.2,1)]';

// 左栏：<768 为 fixed drawer（左侧滑入）；≥768 回到 grid 第一列。
// 开/关用互斥 class 切换（同一时刻只有一个 translate 类生效，避免
// Tailwind 类间排序不确定性），断点类负责覆盖。
// staticVisible = md 及以上（grid 常驻列，drawer 信号为 false 也不 inert）。
// md 用 relative 而不是 static：z-40 只对定位元素生效，static 会让
// 768–1279px 右 drawer 打开时的全屏遮罩（z-30）盖住常驻左栏
// （ui.md §二「中等宽度：左栏保留」），relative 使左栏保持在遮罩之上。
// 内容为 SidebarNav（Lists.tsx 组装导出：品牌 → 新对话 → 工作区 →
// 实例 → 会话 → 底部连接条）。
function NavigationSidebar(props: {
  open: () => boolean;
  staticVisible: () => boolean;
  ref?: HTMLDivElement | undefined;
}) {
  return (
    <aside
      ref={props.ref}
      aria-label="导航"
      inert={!props.open() && !props.staticVisible()}
      class={`fixed inset-y-0 left-0 z-40 w-[min(88vw,320px)] overflow-y-auto border-r border-[var(--border-subtle)] bg-[var(--sidebar-bg)] ${drawerCls} ${
        props.open() ? 'translate-x-0' : '-translate-x-full'
      } md:relative md:w-[var(--sidebar-w)] md:translate-x-0`}
    >
      <SidebarNav />
    </aside>
  );
}

// 右栏：<1280 为 fixed drawer（右侧滑入）；≥1280 回到 grid 第三列（static）。
// staticVisible = xl 及以上（grid 常驻列，drawer 信号为 false 也不 inert）。
function StatusSidebar(props: {
  open: () => boolean;
  staticVisible: () => boolean;
  ref?: HTMLDivElement | undefined;
}) {
  return (
    <aside
      ref={props.ref}
      aria-label="状态"
      inert={!props.open() && !props.staticVisible()}
      class={`fixed inset-y-0 right-0 z-40 w-[min(88vw,320px)] overflow-y-auto border-l border-[var(--border-subtle)] bg-[var(--rail-bg)] ${drawerCls} ${
        props.open() ? 'translate-x-0' : 'translate-x-full'
      } xl:static xl:w-[var(--rail-w)] xl:translate-x-0`}
    >
      <StatusRail />
    </aside>
  );
}

// 中间对话工作区：ChatView（ChatHeader toolbar 自带中窄屏 drawer 开关，
// ui.md §四.5；props 透传在 F3 落地后临时 Launcher 行已移除）。
function ConversationWorkspace(props: {
  onOpenNavigation: () => void;
  onOpenStatus: () => void;
}) {
  return (
    <main class="flex min-h-0 flex-col overflow-hidden">
      <ChatView
        onOpenNavigation={props.onOpenNavigation}
        onOpenStatus={props.onOpenStatus}
      />
    </main>
  );
}

export function AppShell() {
  const [leftOpen, setLeftOpen] = createSignal(false);
  const [rightOpen, setRightOpen] = createSignal(false);
  // 断点状态（与 Tailwind 默认 md=48rem / xl=80rem 一致，即 ui.md 的
  // 768px / 1280px 分档）：左右栏处于 grid 常驻列时豁免 drawer inert。
  const [mdUp, setMdUp] = createSignal(false);
  const [xlUp, setXlUp] = createSignal(false);

  // drawer 焦点管理（§3.10）：drawer 覆盖形态下打开时把焦点移入 drawer
  // 内首个可聚焦元素（本实现 drawer 无独立关闭按钮，以首元素代替；
  // 打开按钮/遮罩/Escape 均可关闭），关闭后焦点返还触发按钮。断点以上
  // 为 grid 常驻列时信号不变，effect 不介入。
  let navRef: HTMLDivElement | undefined;
  let railRef: HTMLDivElement | undefined;
  let lastTrigger: HTMLElement | null = null;

  // 首个可聚焦元素：与 drawer 内交互控件一致（button/input/textarea/
  // [tabindex>=0]），tabindex="-1" 的辅助锚点不算。
  function focusFirst(el: HTMLDivElement | undefined) {
    el?.querySelector<HTMLElement>(
      'button, [href], input, textarea, select, [tabindex]:not([tabindex="-1"])',
    )?.focus();
  }

  // 焦点管理单一 effect（顺序判断避免两个 drawer 信号竞争）：打开时焦点
  // 进入对应 drawer 首元素；两个 drawer 都关闭时（遮罩/Escape/触发按钮）
  // 焦点返还最后记录的触发按钮。
  createEffect(() => {
    if (leftOpen()) {
      focusFirst(navRef);
      return;
    }
    if (rightOpen()) {
      focusFirst(railRef);
      return;
    }
    if (lastTrigger) {
      lastTrigger.focus();
      lastTrigger = null;
    }
  });

  // 打开时先记录触发元素（click 事件中 activeElement 即按钮本身），
  // 关闭后由上方 effect 返还焦点。
  const openLeft = () => {
    lastTrigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setLeftOpen(true);
  };
  const openRight = () => {
    lastTrigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setRightOpen(true);
  };

  // Escape 全关（任一打开即触发）；与 ChatHeader 的 document 级 Escape 并存。
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      setLeftOpen(false);
      setRightOpen(false);
    };
    document.addEventListener('keydown', onKey);
    onCleanup(() => document.removeEventListener('keydown', onKey));
  });

  // 断点监听：决定 drawer 关闭时是否 inert（静态常驻列不 inert）。
  onMount(() => {
    const mqMd = window.matchMedia('(min-width: 48rem)');
    const mqXl = window.matchMedia('(min-width: 80rem)');
    setMdUp(mqMd.matches);
    setXlUp(mqXl.matches);
    const onMd = (e: MediaQueryListEvent) => setMdUp(e.matches);
    const onXl = (e: MediaQueryListEvent) => setXlUp(e.matches);
    mqMd.addEventListener('change', onMd);
    mqXl.addEventListener('change', onXl);
    onCleanup(() => {
      mqMd.removeEventListener('change', onMd);
      mqXl.removeEventListener('change', onXl);
    });
  });

  return (
    <div class="grid h-dvh grid-cols-1 overflow-hidden bg-[var(--app-bg)] md:grid-cols-[var(--sidebar-w)_minmax(0,1fr)] xl:grid-cols-[var(--sidebar-w)_minmax(0,1fr)_var(--rail-w)]">
      <NavigationSidebar open={leftOpen} staticVisible={mdUp} ref={navRef} />
      <ConversationWorkspace
        onOpenNavigation={openLeft}
        onOpenStatus={openRight}
      />
      <StatusSidebar open={rightOpen} staticVisible={xlUp} ref={railRef} />
      {/* 遮罩可见性绑定断点：只在 drawer 实际处于覆盖形态时渲染。
          窄屏（<md）左右 drawer 均需遮罩；768–1279px 仅右 drawer 需要；
          ≥1280px 两侧均为 grid 常驻列，遮罩永不显示——否则跨断点拉伸
          窗口（如窄屏打开 drawer 后拖宽）会留下 fixed inset-0 遮罩盖住
          全部三栏（aside 在断点以上为 grid 列，z-40 不参与对比）。 */}
      <Show when={(leftOpen() && !mdUp()) || (rightOpen() && !xlUp())}>
        <div
          aria-hidden="true"
          onClick={() => {
            setLeftOpen(false);
            setRightOpen(false);
          }}
          class="fixed inset-0 z-30 bg-[var(--scrim)]"
        />
      </Show>
    </div>
  );
}
