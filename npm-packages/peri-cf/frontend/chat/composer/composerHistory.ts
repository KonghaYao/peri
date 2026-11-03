import type { ChatDisplayState } from '../runtime';

/** 本会话已提交的用户消息，按时间从旧到新；空消息不进入召回。 */
export function deriveHistoryPrompts(state: ChatDisplayState | null): string[] {
  if (!state) return [];
  return state.entries.flatMap((entry) => {
    if (entry.role !== 'user') return [];
    const text = entry.blocks
      .flatMap((block) => (block.type === 'text' ? [block.text] : []))
      .join('\n')
      .trim();
    return text ? [text] : [];
  });
}

export interface HistoryStep {
  /** 召回位置；-1 表示退出召回。 */
  index: number;
  text: string;
}

/**
 * 历史召回游标：`-1` 表示未召回。
 * - previous：空历史返回 null；未召回时取最新一条，否则向更旧移动；
 * - next：未召回返回 null；越过最新一条时返回 `{ index: -1, text: '' }` 清空。
 */
export function stepHistory(
  prompts: readonly string[],
  index: number,
  direction: 'previous' | 'next',
): HistoryStep | null {
  if (prompts.length === 0) return null;
  if (direction === 'previous') {
    if (index < 0) return { index: prompts.length - 1, text: prompts[prompts.length - 1]! };
    const next = Math.max(0, index - 1);
    return { index: next, text: prompts[next]! };
  }
  if (index < 0) return null;
  if (index >= prompts.length - 1) return { index: -1, text: '' };
  const next = index + 1;
  return { index: next, text: prompts[next]! };
}

export interface HistoryKeyEvent {
  key: string;
  shiftKey: boolean;
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  isComposing: boolean;
  keyCode: number;
}

/** 只接受裸 ArrowUp/ArrowDown；IME、Shift 选择与组合键保留浏览器默认行为。 */
export function historyRecallDirection(event: HistoryKeyEvent): 'previous' | 'next' | null {
  if (event.isComposing || event.keyCode === 229) return null;
  if (event.shiftKey || event.altKey || event.ctrlKey || event.metaKey) return null;
  if (event.key === 'ArrowUp') return 'previous';
  if (event.key === 'ArrowDown') return 'next';
  return null;
}
