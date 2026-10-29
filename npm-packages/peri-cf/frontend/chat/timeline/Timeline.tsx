import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';
import { LegendList, type LegendListRef } from '@legendapp/list/react';
import type { ChatLoadState, ChatDisplayState, ToolPayloadReader } from '../runtime';
import type { OpenUIActionEvent } from '../openui';
import { deriveTimeline } from './deriveTimeline';
import { isTimelinePinned, type ScrollMetrics } from './timelineFollow';
import {
  rememberTimelinePosition, restoreTimelinePosition,
  type TimelinePosition, type TimelineRestoreProps,
} from './timelinePositions';
import { AssistantMessageRow } from './rows/AssistantMessageRow';
import { ErrorRow } from './rows/ErrorRow';
import { TurnFoldRow } from './rows/TurnFoldRow';
import { UnknownRow } from './rows/UnknownRow';
import { UserMessageRow } from './rows/UserMessageRow';
import { WorkGroupRow } from './rows/WorkGroupRow';
import { WorkingRow } from './rows/WorkingRow';
import type { TimelineRow } from './types';

function TimelineSkeleton() {
  return (
    <div className="mx-auto flex w-full max-w-[46rem] flex-col gap-4 px-4 pt-8" data-slot="peri-timeline-skeleton">
      {[0, 1, 2].map((index) => (
        <div key={index} className="flex flex-col gap-2">
          <div className="bg-muted h-2.5 w-24 animate-pulse rounded" />
          <div className="bg-muted h-3 w-full animate-pulse rounded" />
          <div className="bg-muted h-3 w-4/5 animate-pulse rounded" />
        </div>
      ))}
    </div>
  );
}

/** 发送结算信号：只有目标会话仍是当前会话时才允许回底，迟到的跨会话结算不得移动视图。 */
export interface TimelineScrollSignal {
  token: number;
  chatId: string;
}

function TimelineRowView({
  row, onToggleTurnFold, payloadReader, onOpenUIAction, actionsEnabled,
}: {
  row: TimelineRow;
  onToggleTurnFold?: (turnId: string, expanded: boolean) => void;
  payloadReader?: ToolPayloadReader;
  onOpenUIAction?: (event: OpenUIActionEvent) => void;
  actionsEnabled?: boolean;
}) {
  switch (row.kind) {
    case 'user':
      return <div className="py-2" data-timeline-row-kind="user" data-timeline-row-id={row.id}>
        <UserMessageRow entry={row.entry} />
      </div>;
    case 'assistant':
      return <div className="py-2" data-timeline-row-kind="assistant" data-timeline-row-id={row.id}>
        <AssistantMessageRow
          text={row.text}
          streaming={row.streaming}
          onOpenUIAction={onOpenUIAction}
          actionsEnabled={actionsEnabled}
        />
      </div>;
    case 'work':
      return <div data-timeline-row-kind="work" data-timeline-row-id={row.id} data-work-failed={row.failed ? 'true' : 'false'}>
        <WorkGroupRow
          items={row.items}
          active={row.active}
          failed={row.failed}
          defaultExpanded={row.defaultExpanded}
          forceExpanded={row.forceExpanded}
          bare={row.bare}
          payloadReader={payloadReader}
        />
      </div>;
    case 'turn-fold':
      return <div
        data-timeline-row-kind="turn-fold"
        data-timeline-row-id={row.id}
        data-turn-status={row.status}
        data-turn-failed={row.failed ? 'true' : 'false'}
      >
        <TurnFoldRow
          label={row.label}
          summary={row.summary}
          expanded={row.expanded}
          failed={row.failed}
          isStatic={row.static === true}
          onToggle={onToggleTurnFold ? () => onToggleTurnFold(row.turnId, row.expanded) : undefined}
        />
      </div>;
    case 'working':
      return <div data-timeline-row-kind="working" data-timeline-row-id={row.id}>
        <WorkingRow startedAt={row.startedAt} />
      </div>;
    case 'error':
      return <div className="py-2" data-timeline-row-kind="error" data-timeline-row-id={row.id}>
        <ErrorRow message={row.message} />
      </div>;
    case 'unknown':
      return <div data-timeline-row-kind="unknown" data-timeline-row-id={row.id}>
        <UnknownRow reason={row.reason} />
      </div>;
  }
}

/** 纯行渲染：滚动与空态由外层负责，便于静态渲染测试。 */
export function TimelineRows({
  rows,
  onToggleTurnFold,
  payloadReader,
  onOpenUIAction,
  actionsEnabled,
}: {
  rows: readonly TimelineRow[];
  onToggleTurnFold?: (turnId: string, expanded: boolean) => void;
  payloadReader?: ToolPayloadReader;
  onOpenUIAction?: (event: OpenUIActionEvent) => void;
  actionsEnabled?: boolean;
}) {
  return <div className="flex flex-col" data-slot="peri-timeline-rows">
    {rows.map((row) => (
      <TimelineRowView
        key={row.id}
        row={row}
        onToggleTurnFold={onToggleTurnFold}
        payloadReader={payloadReader}
        onOpenUIAction={onOpenUIAction}
        actionsEnabled={actionsEnabled}
      />
    ))}
  </div>;
}

interface TimelineProps {
  state: ChatDisplayState | null;
  /** 独立读面状态：错误横幅被关闭后，失败态仍然成立。 */
  loadState: ChatLoadState;
  /** 同步失败原因：没有内容时显示错误态，而不是永久骨架。 */
  error?: string;
  /** 会话加载失败且可重试时的入口。 */
  onRetry?: () => void;
  /** 无内容时的自定义空态（欢迎页由外层注入，避免 timeline 依赖视图模块）。 */
  emptyContent?: ReactNode;
  /** composer 浮层高度：内容底部留白，避免被输入框盖住。 */
  bottomInset: number;
  /** 发送会话的结算信号；事件到达时只有目标会话仍是当前会话才回底。 */
  scrollSignal: TimelineScrollSignal | null;
  /** 工具结果全文按需读取；缺席时只展示 inline 结果或预览。 */
  payloadReader?: ToolPayloadReader;
  /** Renderer 内 Button 的 action；由 ChatShell 转成发送/开链接。 */
  onOpenUIAction?: (event: OpenUIActionEvent) => void;
  /** 聊天忙或未就绪时禁用 OpenUI 表单/按钮。 */
  actionsEnabled?: boolean;
}

/** web 事件形状；LegendList 的 web 实现提供 NativeSyntheticEvent 兼容原生事件。 */
interface TimelineScrollEvent {
  nativeEvent: {
    contentOffset: { y: number };
    contentSize: { height: number };
    layoutMeasurement: { height: number };
  };
}

/**
 * 自有时间线：全量 ChatDisplayState → 行，LegendList 只挂载窗口内行。
 * 默认贴底，用户上滚后不被流式内容拉回；位置按 chatId 保存在内存中。
 */
export function Timeline({
  state, loadState, error, onRetry, emptyContent, bottomInset, scrollSignal, payloadReader,
  onOpenUIAction, actionsEnabled,
}: TimelineProps) {
  const [expandedTurns, setExpandedTurns] = useState<ReadonlySet<string>>(() => new Set());
  const [collapsedTurns, setCollapsedTurns] = useState<ReadonlySet<string>>(() => new Set());
  /** 会话身份（用于发送信号匹配）与绑定身份（用于滚动/折叠分代）。 */
  const chatKey = state?.chat.id ?? null;
  const followKey = state ? `${state.chat.id}@${state.bindingRevision ?? 'pending'}` : null;

  // 切换会话或换绑代次时，折叠与滚动位置都不跨代复用。
  useEffect(() => {
    setExpandedTurns(new Set());
    setCollapsedTurns(new Set());
  }, [followKey]);

  const toggleTurnFold = useCallback((turnId: string, expanded: boolean) => {
    if (expanded) {
      setExpandedTurns((previous) => {
        const next = new Set(previous);
        next.delete(turnId);
        return next;
      });
      setCollapsedTurns((previous) => new Set(previous).add(turnId));
    } else {
      setCollapsedTurns((previous) => {
        const next = new Set(previous);
        next.delete(turnId);
        return next;
      });
      setExpandedTurns((previous) => new Set(previous).add(turnId));
    }
  }, []);

  const rows = useMemo(
    () => deriveTimeline(state, { expandedTurnIds: expandedTurns, collapsedTurnIds: collapsedTurns }),
    [state, expandedTurns, collapsedTurns],
  );
  /** 折叠状态变化时强制列表重绘；虚拟列表可能按 key 缓存已渲染项。 */
  const foldSignature = useMemo(
    () => rows
      .filter((row) => row.kind === 'turn-fold')
      .map((row) => `${row.turnId}:${row.expanded ? 1 : 0}`)
      .join('|'),
    [rows],
  );

  const listRef = useRef<LegendListRef>(null);
  const positions = useRef(new Map<string, TimelinePosition>());
  const pinnedRef = useRef(true);
  const [pinned, setPinned] = useState(true);
  const firstVisibleRow = useRef<string | null>(null);
  /** 用户意图：只有滚动位置上移才取消贴底；内容增长不算。 */
  const lastScrollTop = useRef(0);
  const scrollInitialized = useRef(false);
  /** 首次布局/恢复完成前忽略滚动事件，避免把初始的 0 当成用户上滚。 */
  const allowScrollUpdates = useRef(false);
  /** 每个会话自己的贴底事实；切换时旧 key 的条目不会被新会话的恢复覆盖。 */
  const pinnedByKey = useRef(new Map<string, boolean>());
  /**
   * 按 key 计算一次恢复参数。只读位置存储，不在 render 阶段写跨 key 的 pinnedRef：
   * 旧会话的 cleanup 必须看到切换前的贴底状态，而不是新会话的恢复结果。
   * 行从空变为非空时重算一次，确保锚点行可用，而不是只退回像素偏移。
   */
  const hasRows = rows.length > 0;
  const restoreProps = useMemo<TimelineRestoreProps>(
    () => restoreTimelinePosition(followKey ? positions.current.get(followKey) : undefined, rows),
    [followKey, hasRows],
  );

  const savePosition = useCallback((metrics: ScrollMetrics) => {
    if (!followKey) return;
    const list = listRef.current;
    const rowId = firstVisibleRow.current;
    const rowTop = rowId && list ? list.getState().positionByKey(rowId) ?? null : null;
    positions.current.set(followKey, rememberTimelinePosition(metrics, rowId, rowTop));
  }, [followKey]);

  // 首次布局/恢复后的第一帧才接受滚动事件；初始的 scrollTop=0 不是用户意图。
  useEffect(() => {
    allowScrollUpdates.current = false;
    scrollInitialized.current = false;
    lastScrollTop.current = 0;
    const frame = requestAnimationFrame(() => {
      allowScrollUpdates.current = true;
    });
    return () => cancelAnimationFrame(frame);
  }, [followKey, restoreProps]);

  const onScroll = useCallback((event: TimelineScrollEvent) => {
    if (!allowScrollUpdates.current) return;
    const { contentOffset, contentSize, layoutMeasurement } = event.nativeEvent;
    const metrics: ScrollMetrics = {
      scrollHeight: contentSize.height,
      scrollTop: contentOffset.y,
      clientHeight: layoutMeasurement.height,
    };
    const atEnd = isTimelinePinned(metrics);
    let next = pinnedRef.current;
    if (!scrollInitialized.current) {
      // 恢复完成后的第一帧以实际位置为准。
      scrollInitialized.current = true;
      next = atEnd;
    } else if (atEnd) {
      next = true;
    } else if (metrics.scrollTop < lastScrollTop.current - 1) {
      // 只有位置上移才算用户离开底部；内容增长导致的“不再贴底”不算。
      next = false;
    }
    lastScrollTop.current = metrics.scrollTop;
    if (followKey) pinnedByKey.current.set(followKey, next);
    if (next !== pinnedRef.current) {
      pinnedRef.current = next;
      setPinned(next);
    }
    // 贴底时由列表与 markAtEnd 维护位置，避免流式期间保存中间锚点。
    if (!next) savePosition(metrics);
  }, [savePosition, followKey]);

  /** 同步记录“当前会话应贴底”的事实；滚动本身可能等到下一帧。 */
  const markAtEnd = useCallback(() => {
    pinnedRef.current = true;
    setPinned(true);
    if (!followKey) return;
    pinnedByKey.current.set(followKey, true);
    positions.current.set(followKey, {
      atEnd: true, rowId: null, offsetWithinRow: 0, scrollOffset: 0,
    });
  }, [followKey]);

  const scrollToEnd = useCallback((animated = false) => {
    void listRef.current?.scrollToEnd({ animated });
  }, []);

  const scrollToBottom = useCallback((animated = false) => {
    markAtEnd();
    scrollToEnd(animated);
  }, [markAtEnd, scrollToEnd]);

  // 进入会话先同步恢复后的贴底状态，必须排在内容增长 effect 之前。
  useLayoutEffect(() => {
    pinnedRef.current = restoreProps.initialScrollAtEnd;
    setPinned(restoreProps.initialScrollAtEnd);
    if (followKey) pinnedByKey.current.set(followKey, restoreProps.initialScrollAtEnd);
  }, [followKey, restoreProps]);

  // 内容增长时只在仍然贴底的情况下跟随；用户上滚后这里不会触碰滚动位置。
  useLayoutEffect(() => {
    if (!pinnedRef.current) return;
    void listRef.current?.scrollToEnd({ animated: false });
  }, [rows]);

  // 发送新消息总是回到底部；跨会话迟到的结算信号必须被忽略，不能移动当前会话视图。
  // 会话切换本身由 initialScrollAtEnd/initialScrollIndex 负责。
  const previousScrollSignal = useRef<TimelineScrollSignal | null>(scrollSignal);
  useEffect(() => {
    const previous = previousScrollSignal.current;
    previousScrollSignal.current = scrollSignal;
    if (!scrollSignal || previous?.token === scrollSignal.token) return;
    if (scrollSignal.chatId !== chatKey) return;
    // 贴底状态与本轮信号同步提交：主线程繁忙时动画帧可能晚到，不能让“回到底部”
    // 按钮在信号已生效后仍然可见。滚动本身仍等一帧，让新内容先完成布局。
    markAtEnd();
    const frame = requestAnimationFrame(() => scrollToEnd(false));
    return () => cancelAnimationFrame(frame);
  }, [scrollSignal, chatKey, markAtEnd, scrollToEnd]);

  // 离开会话时只记录该 key 自己的贴底事实；上滚位置已由滚动事件保存。
  // cleanup 捕获旧 key，并读取按 key 保存的状态，因此切到 null/加载态不会覆盖旧位置。
  useEffect(() => {
    const key = followKey;
    return () => {
      if (!key || pinnedByKey.current.get(key) !== true) return;
      positions.current.set(key, { atEnd: true, rowId: null, offsetWithinRow: 0, scrollOffset: 0 });
    };
  }, [followKey]);

  const renderItem = useCallback(({ item }: { item: TimelineRow }) => (
    <TimelineRowView
      row={item}
      onToggleTurnFold={toggleTurnFold}
      payloadReader={payloadReader}
      onOpenUIAction={onOpenUIAction}
      actionsEnabled={actionsEnabled}
    />
  ), [toggleTurnFold, payloadReader, onOpenUIAction, actionsEnabled]);

  const notReady = state !== null && state.readiness !== 'ready';
  const noContent = !notReady && rows.length === 0;
  const syncFailed = noContent && loadState === 'failed';
  const loading = noContent && loadState === 'loading';

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      {notReady || noContent ? (
        <div
          className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain [scrollbar-gutter:stable]"
          data-slot="peri-timeline"
          data-timeline-state={state?.readiness ?? 'empty'}
        >
          {notReady ? (
            <div className="text-muted-foreground flex h-full items-center justify-center px-6 text-sm">
              {state?.readiness === 'unsupported' ? '会话使用了不受支持的数据版本。' : '会话数据尚未就绪。'}
            </div>
          ) : syncFailed ? (
            <div
              role="alert"
              data-slot="peri-timeline-error"
              className="text-muted-foreground flex h-full flex-col items-center justify-center gap-3 px-6 text-sm"
            >
              <span>{error ? `会话同步失败：${error}` : '会话加载失败，请重新加载。'}</span>
              {onRetry && (
                <button
                  type="button"
                  className="border-border hover:bg-muted rounded-md border px-2.5 py-1 text-xs"
                  onClick={onRetry}
                >
                  重新加载
                </button>
              )}
            </div>
          ) : loading ? (
            <TimelineSkeleton />
          ) : emptyContent ? (
            <div className="flex min-h-full items-center justify-center px-4" style={{ paddingBottom: bottomInset }}>
              {emptyContent}
            </div>
          ) : (
            <div className="text-muted-foreground/70 flex h-full items-center justify-center px-6 text-sm">
              发送一条消息开始对话。
            </div>
          )}
        </div>
      ) : (
        <LegendList<TimelineRow>
          ref={listRef}
          key={followKey ?? 'timeline'}
          data={rows}
          dataKey={followKey ?? 'timeline'}
          keyExtractor={(row) => row.id}
          getItemType={(row) => row.kind}
          renderItem={renderItem}
          estimatedItemSize={90}
          estimatedListSize={{ height: 640, width: 800 }}
          drawDistance={600}
          // 行组件持有本地展开状态；复用容器会在内容切换时保留旧 DOM，先关闭。
          recycleItems={false}
          maintainScrollAtEnd={pinned}
          extraData={foldSignature}
          {...restoreProps}
          onScroll={onScroll}
          onFirstVisibleItemChanged={({ key }) => { firstVisibleRow.current = key; }}
          className="min-h-0 flex-1 overflow-y-auto overscroll-y-contain [scrollbar-gutter:stable]"
          contentContainerClassName="mx-auto w-full max-w-[46rem] px-4 pt-6"
          contentContainerStyle={{ paddingBottom: bottomInset + 24 }}
          data-slot="peri-timeline"
          data-timeline-state={state?.readiness ?? 'empty'}
        />
      )}

      {!pinned && !noContent && (
        <div className="pointer-events-none absolute inset-x-0 z-30 flex justify-center" style={{ bottom: bottomInset + 12 }}>
          <button
            type="button"
            className="bg-background/95 text-foreground hover:bg-muted pointer-events-auto inline-flex items-center gap-1 rounded-full border px-3 py-1 text-xs shadow-sm backdrop-blur"
            onClick={() => scrollToBottom(true)}
          >
            <ChevronDown className="size-3.5" aria-hidden="true" />
            回到底部
          </button>
        </div>
      )}
    </div>
  );
}
