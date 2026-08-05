// log.ts 单元测试：事件白名单结构与 channel 截断 hash。
// Worker 侧 console 不经过 miniflare Log（直连 stdout），故在进程内
// 拦截 console.log 验证 safeLog 输出格式。

import { afterEach, describe, expect, it } from "bun:test";
import { channelHash, safeLog } from "./log";

const captured: string[] = [];
const originalLog = console.log;

afterEach(() => {
	console.log = originalLog;
	captured.length = 0;
});

function captureLogs(fn: () => void): string[] {
	console.log = (msg?: unknown) => {
		captured.push(String(msg));
	};
	fn();
	return captured;
}

describe("safeLog", () => {
	it("输出 JSON 事件行且仅含白名单字段", () => {
		const lines = captureLogs(() => safeLog("join", "ok", { codeShard: "7", channelHash: "abc12345" }));
		expect(lines).toHaveLength(1);
		const parsed = JSON.parse(lines[0]!) as Record<string, string>;
		expect(parsed.event).toBe("join");
		expect(parsed.outcome).toBe("ok");
		expect(parsed.codeShard).toBe("7");
		expect(parsed.channelHash).toBe("abc12345");
		expect(Object.keys(parsed).sort()).toEqual(["channelHash", "codeShard", "event", "outcome"]);
	});

	it("无 extras 时仅 event/outcome", () => {
		const lines = captureLogs(() => safeLog("cleanup", "ok"));
		const parsed = JSON.parse(lines[0]!) as Record<string, string>;
		expect(Object.keys(parsed).sort()).toEqual(["event", "outcome"]);
	});

	it("channelHash 只输出截断 SHA-256（8 字符），不含原文", async () => {
		const hash = await channelHash("SOME-CHANNEL-ID-abcdef");
		expect(hash).toMatch(/^[A-Za-z0-9_-]{8}$/);
		expect(hash).not.toContain("SOME-CHANNEL");
		expect(hash).not.toBe("SOME-CHANNEL-ID-abcdef");
	});

	it("不同 channel_id 产生不同 hash", async () => {
		const a = await channelHash("channel-a");
		const b = await channelHash("channel-b");
		expect(a).not.toBe(b);
	});
});
