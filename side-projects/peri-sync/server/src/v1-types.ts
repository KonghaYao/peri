/// <reference types="@cloudflare/workers-types" />
// 新协议（v1）公开 HTTP DTO 类型（03-plan Slice 2 契约）。
// 旧 WS 消息类型保留在 types.ts（Slice 4 前不得删除）。

/** channel 状态机（03-plan：created → paired → ready → transferring → 终态）。 */
export type ChannelState =
	| "created"
	| "paired"
	| "ready"
	| "transferring"
	| "confirmed"
	| "revoked"
	| "expired";

export type HandshakeRole = "sender" | "receiver";

export interface CreateChannelRequest {
	/** sender 本地 CSPRNG 16 字节 base64url-no-pad。 */
	channel_id: string;
	device_id: string;
	expected_device_id: string;
	/** create 时登记的 expected receiver Ed25519 公钥（join 验签密钥）。 */
	expected_ed_pub: string;
	sender_ed_pub: string;
	sender_x_pub: string;
}

export interface CreateChannelResponse {
	channel_id: string;
	expires_at: number;
}

export interface JoinChannelRequest {
	/** 用户输入的显示格式码（服务端归一化）。 */
	code: string;
	device_id: string;
	ed_pub: string;
	x_pub: string;
}

export interface RegisterCodeRequest {
	/** sender 生成的 8 字符显示格式码（服务端归一化）。 */
	code: string;
	/** 客户端 epoch（unix_secs / 30），仅入签名/审计，服务端不强制对齐。 */
	epoch: number;
}

export interface HandshakeRequest {
	/** opaque Noise blob（base64url-no-pad）；省略/null 表示仅拉取对端消息。 */
	msg?: string | null;
}

export interface HandshakeResponse {
	/** 对端已存消息（base64url-no-pad）或 null。 */
	peer_msg: string | null;
	state: ChannelState;
	expires_at: number;
}

export interface UploadPartRequest {
	part_index: number;
	/** AEAD envelope 密文（base64url-no-pad），≤ 64 KiB。 */
	ciphertext: string;
}

export interface UploadPartResponse {
	part_index: number;
	size: number;
}

export interface LookupResponse {
	channel_id: string;
	valid_until: number;
}

export interface ChannelStateResponse {
	state: ChannelState;
	expires_at: number;
}

// ─── 内部（DO 间）协议 ─────────────────────────────────────────────────────

export interface RegisterCodeInternal {
	code: string;
	channel_id: string;
	now: number;
}

export interface VerifyCodeInternal {
	code: string;
	channel_id: string;
	now: number;
}

export interface VerifyCodeInternalResponse {
	valid: boolean;
}

export interface DeleteCodesInternal {
	channel_id: string;
}

// ─── 响应辅助 ──────────────────────────────────────────────────────────────

export function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { "content-type": "application/json; charset=utf-8" },
	});
}

/** 错误体不泄露细节：统一 `{ error: <code> }`。 */
export function jsonError(code: string, status: number, retryAfter?: number): Response {
	const headers: Record<string, string> = { "content-type": "application/json; charset=utf-8" };
	if (retryAfter !== undefined) headers["retry-after"] = String(Math.ceil(retryAfter));
	return new Response(JSON.stringify({ error: code }), { status, headers });
}

export const ERR = {
	badRequest: (): Response => jsonError("BAD_REQUEST", 400),
	invalidSignature: (): Response => jsonError("INVALID_SIGNATURE", 401),
	forbidden: (): Response => jsonError("FORBIDDEN", 403),
	notFound: (): Response => jsonError("NOT_FOUND", 404),
	conflict: (): Response => jsonError("CONFLICT", 409),
	collision: (): Response => jsonError("COLLISION", 409),
	tooLarge: (): Response => jsonError("TOO_LARGE", 413),
	rateLimited: (retryAfter: number): Response => jsonError("RATE_LIMITED", 429, retryAfter),
} as const;

export const NO_CONTENT = new Response(null, { status: 204 });
