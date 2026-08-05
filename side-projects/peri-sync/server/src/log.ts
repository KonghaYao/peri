/// <reference types="@cloudflare/workers-types" />
// 安全日志：事件白名单（03-plan §1.7 冻结语义）。
//
// 绝不记录：同步码、URI、Authorization/签名、payload、verifier、完整
// channel_id（仅截断 hash）、密钥材料。事件仅 `{event, outcome}` + 可选
// `code_shard`（码首字符）与 `channel_hash`（SHA-256 截断 8 字符）。

import { b64urlEncode, utf8Bytes } from "./canonical";

/** 完整 channel_id 只以截断 SHA-256 进入日志。 */
export async function channelHash(channelId: string): Promise<string> {
	const bytes = utf8Bytes(channelId);
	const digest = await crypto.subtle.digest(
		"SHA-256",
		bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer,
	);
	return b64urlEncode(new Uint8Array(digest)).slice(0, 8);
}

export type SafeEvent =
	| "create"
	| "join"
	| "ready"
	| "confirm"
	| "revoke"
	| "expire"
	| "upload"
	| "download"
	| "lookup"
	| "code"
	| "handshake"
	| "cleanup";

export type SafeOutcome = "ok" | "collision" | "denied" | "expired" | "error";

/** 结构化安全日志；extras 仅允许白名单字段，调用方禁止传入敏感值。 */
export function safeLog(
	event: SafeEvent,
	outcome: SafeOutcome,
	extras?: { codeShard?: string; channelHash?: string },
): void {
	const entry: Record<string, string> = { event, outcome };
	if (extras?.codeShard !== undefined) entry.codeShard = extras.codeShard;
	if (extras?.channelHash !== undefined) entry.channelHash = extras.channelHash;
	console.log(JSON.stringify(entry));
}
