import { describe, expect, test } from 'bun:test';
import { isTimelinePinned, TIMELINE_FOLLOW_THRESHOLD_PX } from './timelineFollow';

const viewport = { scrollHeight: 1000, scrollTop: 600, clientHeight: 400 };

describe('timeline follow policy', () => {
  test('treats the 24px bottom band as pinned', () => {
    expect(isTimelinePinned(viewport)).toBe(true);
    expect(isTimelinePinned({ ...viewport, scrollTop: 576 })).toBe(true);
    expect(isTimelinePinned({ ...viewport, scrollTop: 575 })).toBe(false);
    expect(TIMELINE_FOLLOW_THRESHOLD_PX).toBe(24);
  });

  test('content growth alone does not move the reading position back to the bottom', () => {
    // 同一 scrollTop 下内容变高只会离底部更远：判定必须保持未贴底。
    expect(isTimelinePinned({ ...viewport, scrollHeight: 1600, scrollTop: 100 })).toBe(false);
  });
});
