/** 贴底阈值：与 t3code 的 24px 一致。 */
export const TIMELINE_FOLLOW_THRESHOLD_PX = 24;

export interface ScrollMetrics {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}

/** 距底部小于阈值即视为贴底；跟随策略本身由 Timeline 持有，这里是唯一判定规则。 */
export function isTimelinePinned(
  metrics: ScrollMetrics, threshold = TIMELINE_FOLLOW_THRESHOLD_PX,
): boolean {
  return metrics.scrollHeight - metrics.scrollTop - metrics.clientHeight <= threshold;
}
