import type { TimelineRow } from './types';
import { isTimelinePinned, type ScrollMetrics } from './timelineFollow';

/** 按 chatId 保留的滚动位置；只存内存，不持久化。 */
export interface TimelinePosition {
  atEnd: boolean;
  rowId: string | null;
  /** 视口顶部与锚点行顶部的像素差。 */
  offsetWithinRow: number;
  scrollOffset: number;
}

/** 滚动事实 → 位置：贴底只记终态，上滚后记锚点行，便于恢复不跳动。 */
export function rememberTimelinePosition(
  metrics: ScrollMetrics, rowId: string | null, rowTop: number | null,
): TimelinePosition {
  const atEnd = isTimelinePinned(metrics);
  return {
    atEnd,
    rowId: atEnd ? null : rowId,
    offsetWithinRow: atEnd || rowTop === null ? 0 : Math.max(0, metrics.scrollTop - rowTop),
    scrollOffset: metrics.scrollTop,
  };
}

export interface TimelineRestoreProps {
  initialScrollAtEnd: boolean;
  initialScrollIndex?: { index: number; viewPosition: number; viewOffset: number };
  initialScrollOffset?: number;
}

/** 位置 → LegendList 初始滚动参数；找不到锚点行时退回像素偏移。 */
export function restoreTimelinePosition(
  position: TimelinePosition | undefined, rows: readonly TimelineRow[],
): TimelineRestoreProps {
  if (!position || position.atEnd) return { initialScrollAtEnd: true };
  const index = position.rowId ? rows.findIndex((row) => row.id === position.rowId) : -1;
  if (index < 0) {
    return { initialScrollAtEnd: false, initialScrollOffset: position.scrollOffset };
  }
  return {
    initialScrollAtEnd: false,
    initialScrollIndex: { index, viewPosition: 0, viewOffset: -position.offsetWithinRow },
  };
}
