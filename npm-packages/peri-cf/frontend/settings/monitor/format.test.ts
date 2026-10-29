import { describe, expect, test } from "bun:test";
import {
  formatBytes,
  formatClock,
  formatDateTime,
  formatDuration,
  formatElapsed,
  formatRelativeTime,
  formatTokens,
  formatWallMs,
} from "./format";

describe("monitor format", () => {
  test("bytes 使用 1024 进制并拒绝非法值", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1 KiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(64 * 1024 * 1024)).toBe("64 MiB");
    expect(formatBytes(-1)).toBe("—");
    expect(formatBytes(Number.NaN)).toBe("—");
  });

  test("duration 组合到秒，零值仍有单位", () => {
    expect(formatDuration(0)).toBe("0 秒");
    expect(formatDuration(5)).toBe("5 秒");
    expect(formatDuration(65)).toBe("1 分 5 秒");
    expect(formatDuration(3725)).toBe("1 小时 2 分 5 秒");
    expect(formatDuration(90_061)).toBe("1 天 1 小时 1 分 1 秒");
    expect(formatDuration(-5)).toBe("—");
  });

  test("elapsed 与本地时钟格式化", () => {
    expect(formatElapsed("2026-10-27T09:00:00.000Z", "2026-10-27T10:00:00.000Z")).toBe("1 小时");
    expect(formatElapsed("bad", "2026-10-27T10:00:00.000Z")).toBe("—");
    const date = new Date(2026, 0, 2, 9, 5, 7);
    expect(formatClock(date)).toBe("09:05:07");
    expect(formatClock(null)).toBe("—");
    expect(formatDateTime(date.toISOString())).toBe("2026-01-02 09:05:07");
    expect(formatDateTime("bad")).toBe("—");
  });

  test("token 千分位且不把不可用显示为 0", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(1_234)).toBe("1,234");
    expect(formatTokens(34_567_890)).toBe("34,567,890");
    expect(formatTokens(null)).toBe("—");
    expect(formatTokens(Number.NaN)).toBe("—");
  });

  test("相对时间与墙钟时长", () => {
    const now = "2026-10-27T10:00:00.000Z";
    expect(formatRelativeTime(null, now)).toBe("—");
    expect(formatRelativeTime("2026-10-27T09:59:58.000Z", now)).toBe("刚刚");
    expect(formatRelativeTime("2026-10-27T09:59:48.000Z", now)).toBe("12 秒前");
    expect(formatRelativeTime("2026-10-27T09:55:00.000Z", now)).toBe("5 分钟前");
    expect(formatRelativeTime("2026-10-27T08:00:00.000Z", now)).toBe("2 小时前");
    expect(formatRelativeTime("2026-10-25T10:00:00.000Z", now)).toBe("2 天前");
    expect(formatRelativeTime("bad", now)).toBe("—");

    expect(formatWallMs(null)).toBe("—");
    expect(formatWallMs(250)).toBe("250 毫秒");
    expect(formatWallMs(1_500)).toBe("1.5 秒");
    expect(formatWallMs(30_000)).toBe("30 秒");
    expect(formatWallMs(120_000)).toBe("2 分");
    expect(formatWallMs(-1)).toBe("—");
  });
});
