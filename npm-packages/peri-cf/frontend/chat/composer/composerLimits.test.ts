import { describe, expect, test } from 'bun:test';
import { CHAT_SEND_TEXT_MAX_LENGTH } from '@peri-code/sdk';
import {
  composerLimitAnnouncement, composerLimitMessage, composerLimitState,
  MAX_COMPOSER_CHARS,
} from './composerLimits';

describe('composer limit state', () => {
  test('uses the SDK send limit as the single authority', () => {
    expect(MAX_COMPOSER_CHARS).toBe(CHAT_SEND_TEXT_MAX_LENGTH);
    expect(MAX_COMPOSER_CHARS).toBe(32_768);
  });

  test('boundaries: near limit shows count, over limit blocks send', () => {
    const below = composerLimitState('x'.repeat(MAX_COMPOSER_CHARS - 1));
    expect(below.overLimit).toBe(false);
    expect(below.nearLimit).toBe(true);
    expect(below.remaining).toBe(1);

    const atLimit = composerLimitState('x'.repeat(MAX_COMPOSER_CHARS));
    expect(atLimit.overLimit).toBe(false);
    expect(atLimit.nearLimit).toBe(true);
    expect(atLimit.remaining).toBe(0);

    const overLimit = composerLimitState('x'.repeat(MAX_COMPOSER_CHARS + 1));
    expect(overLimit.overLimit).toBe(true);
    expect(overLimit.remaining).toBe(-1);
    expect(composerLimitMessage(overLimit)).toContain('已超出 1 字');
  });

  test('is quiet below 80% and counts UTF-16 units like the SDK', () => {
    expect(composerLimitState('').showCount).toBe(false);
    expect(composerLimitState('普通消息').showCount).toBe(false);
    const emoji = composerLimitState('😀');
    expect(emoji.length).toBe(2);
    expect(emoji.overLimit).toBe(false);
  });

  test('announces only threshold transitions', () => {
    expect(composerLimitAnnouncement(composerLimitState('hi'))).toBe('');
    expect(composerLimitAnnouncement(composerLimitState('x'.repeat(MAX_COMPOSER_CHARS))))
      .toContain('接近上限');
    expect(composerLimitAnnouncement(composerLimitState('x'.repeat(MAX_COMPOSER_CHARS + 2))))
      .toContain('超出上限');
  });
});
