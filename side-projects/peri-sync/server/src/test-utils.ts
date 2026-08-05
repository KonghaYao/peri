// 测试工具（bun:test + miniflare）：模拟 Slice 3 客户端的设备签名与 HTTP 流程。
// 仅测试使用，不进入 Worker bundle。

import { Miniflare } from "miniflare";
import { buildSync } from "esbuild";
import type { Readable } from "node:stream";
import { transcript, b64urlEncode } from "./canonical";

export interface TestDevice {
	deviceId: string;
	edPubB64: string;
	xPubB64: string;
	sign: (msg: string) => Promise<string>;
}

function utf8(s: string): Uint8Array {
	return new TextEncoder().encode(s);
}

function randomB64(n: number): string {
	const bytes = new Uint8Array(n);
	crypto.getRandomValues(bytes);
	return b64urlEncode(bytes);
}

/** 生成模拟设备：Ed25519 签名身份 + 随机 X25519 公钥（仅测试）。 */
export async function makeDevice(): Promise<TestDevice> {
	const kp = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
	const raw = new Uint8Array(await crypto.subtle.exportKey("raw", kp.publicKey));
	return {
		deviceId: randomB64(16),
		edPubB64: b64urlEncode(raw),
		xPubB64: randomB64(32),
		sign: async (msg: string) => {
			const bytes = utf8(msg);
			const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
			const sig = await crypto.subtle.sign({ name: "Ed25519" }, kp.privateKey, buf);
			return b64urlEncode(new Uint8Array(sig));
		},
	};
}

/** 构造 PeriSig Authorization 头（客户端视角）。 */
export async function periSig(
	device: TestDevice,
	op: string,
	fields: string[],
	ts: number,
): Promise<string> {
	const msg = transcript(op, fields, ts);
	const sig = await device.sign(msg);
	return `PeriSig ${device.deviceId} ${ts} ${sig}`;
}

export function nowSecs(): number {
	return Math.floor(Date.now() / 1000);
}

export function sleep(ms: number): Promise<void> {
	return new Promise((r) => setTimeout(r, ms));
}

/**
 * 将 workerd 的 stdout/stderr（worker 侧 console.log 直连 stdout，不经
 * miniflare Log）按行转发到 sink，供日志白名单断言。
 */
function pipeRuntimeToSink(stdout: Readable, stderr: Readable, sink: (msg: string) => void): void {
	for (const stream of [stdout, stderr]) {
		stream.setEncoding("utf8");
		let buf = "";
		stream.on("data", (chunk: string) => {
			buf += chunk;
			let i: number;
			while ((i = buf.indexOf("\n")) >= 0) {
				const line = buf.slice(0, i);
				buf = buf.slice(i + 1);
				if (line.trim().length > 0) sink(line);
			}
		});
		stream.on("end", () => {
			if (buf.trim().length > 0) sink(buf);
		});
	}
}

let bundled: string | null = null;

/** esbuild 打包 Worker（miniflare 不直接解析 TS；cloudflare:workers 保持外部）。 */
function workerScript(): string {
	if (bundled === null) {
		const res = buildSync({
			entryPoints: ["src/cf-worker.ts"],
			bundle: true,
			format: "esm",
			platform: "browser",
			target: "es2022",
			external: ["cloudflare:workers"],
			write: false,
			logLevel: "silent",
		});
		bundled = res.outputFiles[0]!.text;
	}
	return bundled;
}

/** 创建 miniflare 实例（功能测试用默认 TTL；alarm 测试传短 TTL bindings）。
 * 传 `logSink` 时捕获 worker 侧 console 输出（safeLog 事件行）供日志白名单
 * 断言——注意 miniflare 自身的 mf:info 请求行含完整 URL（channel_id/code），
 * 不属于 Worker 日志，故不接入 logSink。 */
export function makeMf(
	bindings: Record<string, unknown> = {},
	opts: { logSink?: (msg: string) => void } = {},
): Miniflare {
	const sink = opts.logSink;
	return new Miniflare({
		modules: true,
		script: workerScript(),
		compatibilityDate: "2026-02-24",
		durableObjects: {
			CHANNEL: { className: "Channel", useSQLite: true },
			CODE_INDEX: { className: "CodeIndex", useSQLite: true },
		},
		r2Buckets: ["PERI_SYNC_PAYLOADS"],
		bindings,
		handleRuntimeStdio: sink
			? (stdout: Readable, stderr: Readable) => pipeRuntimeToSink(stdout, stderr, sink)
			: undefined,
	});
}
