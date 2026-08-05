/// <reference types="@cloudflare/workers-types" />
// CodeIndex DO（r2-encrypted-transfer v1）。
//
// SQLite `codes` 表：归一化 code 主键 → channel_id + expires_at（码注册时刻
// 起有效 60s）；`idx_codes_expiry` 索引；惰性清理 + alarm。lookup 仅 IP 限流
// （匿名），仅有效码行返回 locator，miss/过期/撤销统一 404（无 oracle）。
//
// 内部端点（/internal/*）仅供 Channel DO 跨 DO 调用，入口白名单不可达：
// - /internal/register：注册码行（撞码 409 / 同 channel 幂等）
// - /internal/verify：join 时校验码行有效性
// - /internal/delete：join/revoke 后删除该 channel 全部码行
// - /v1/channels：create 前置防线（H1）——完整格式校验 + per-IP 限流 +
//   sender_ed_pub 验签全部通过后才分配 Channel DO stub 并转发；
//   非法/验签失败直接 400/401，绝不产生 Channel DO。device 限流保留
//   但自报 device_id 可换键绕过，per-IP 才是不可伪造的唯一防线。

import { DurableObject } from "cloudflare:workers";
import { isValidChannelId, isValidDeviceId, isValidPubKey, normalizeSyncCode } from "./canonical";
import { verifyPeriSig } from "./crypto";
import { createRateTables, rateLimit } from "./rate";
import { limits, type Limits } from "./limits";
import { channelHash, safeLog } from "./log";
import {
	json,
	jsonError,
	ERR,
	type CreateChannelRequest,
	type RegisterCodeInternal,
	type VerifyCodeInternal,
	type VerifyCodeInternalResponse,
} from "./v1-types";

export interface CodeIndexEnv {
	CHANNEL: DurableObjectNamespace;
	CODE_INDEX: DurableObjectNamespace;
	PERI_SYNC_PAYLOADS: R2Bucket;
	[key: string]: unknown;
}

interface CodeRow extends Record<string, SqlStorageValue> {
	code: string;
	channel_id: string;
	expires_at: number;
}

const ROUTES = ["lookup", "create", "create-ip"];

export class CodeIndex extends DurableObject {
	private readonly L: Limits;
	private readonly init: Promise<void>;

	constructor(ctx: DurableObjectState, env: CodeIndexEnv) {
		super(ctx, env);
		this.L = limits(env);
		this.init = ctx.blockConcurrencyWhile(async () => {
			this.ctx.storage.sql.exec(
				`CREATE TABLE IF NOT EXISTS codes (
					code TEXT PRIMARY KEY,
					channel_id TEXT NOT NULL,
					expires_at INTEGER NOT NULL
				)`,
			);
			this.ctx.storage.sql.exec(
				"CREATE INDEX IF NOT EXISTS idx_codes_expiry ON codes(expires_at)",
			);
			createRateTables(this.ctx.storage.sql, ROUTES);
			// 惰性 alarm：定期清理过期码行与 rate 行。
			await this.ctx.storage.setAlarm(Date.now() + 60_000);
		});
	}

	async fetch(request: Request): Promise<Response> {
		await this.init;
		const url = new URL(request.url);
		const now = Math.floor(Date.now() / 1000);

		if (request.method === "POST" && url.pathname === "/v1/channels") {
			return this.handleCreate(request, now);
		}
		if (request.method === "POST" && url.pathname.startsWith("/v1/code/")) {
			const m = url.pathname.match(/^\/v1\/code\/([^/]+)\/lookup$/);
			if (!m) return ERR.notFound();
			return this.handleLookup(m[1], now, request.headers.get("CF-Connecting-IP"));
		}
		if (request.method === "POST" && url.pathname === "/internal/register") {
			return this.handleRegister(request, now);
		}
		if (request.method === "POST" && url.pathname === "/internal/verify") {
			return this.handleVerify(request, now);
		}
		if (request.method === "POST" && url.pathname === "/internal/delete") {
			return this.handleDelete(request);
		}
		return ERR.notFound();
	}

	async alarm(): Promise<void> {
		await this.init;
		const sql = this.ctx.storage.sql;
		const now = Math.floor(Date.now() / 1000);
		sql.exec("DELETE FROM codes WHERE expires_at < ?", now);
		for (const route of ROUTES) {
			sql.exec(`DELETE FROM rate_${route} WHERE window_start < ?`, now - 3600);
		}
		safeLog("cleanup", "ok");
		await this.ctx.storage.setAlarm(Date.now() + 60_000);
	}

	// ─── create：前置防线（H1）通过后才转发 Channel DO ──────────────────────

	private async handleCreate(request: Request, now: number): Promise<Response> {
		let body: CreateChannelRequest;
		try {
			body = (await request.json()) as CreateChannelRequest;
		} catch {
			return ERR.badRequest();
		}
		// H1(a)：channel_id/device_id/pubkey 完整格式校验，非法直接 400，
		// 不分配 Channel DO stub。
		if (
			typeof body.channel_id !== "string" ||
			typeof body.device_id !== "string" ||
			typeof body.expected_device_id !== "string" ||
			typeof body.expected_ed_pub !== "string" ||
			typeof body.sender_ed_pub !== "string" ||
			typeof body.sender_x_pub !== "string"
		) {
			return ERR.badRequest();
		}
		if (
			!isValidChannelId(body.channel_id) ||
			!isValidDeviceId(body.device_id) ||
			!isValidDeviceId(body.expected_device_id)
		) {
			return ERR.badRequest();
		}
		if (
			!isValidPubKey(body.expected_ed_pub) ||
			!isValidPubKey(body.sender_ed_pub) ||
			!isValidPubKey(body.sender_x_pub)
		) {
			return ERR.badRequest();
		}
		// H1(c)：per-IP 限流（CF-Connecting-IP）。自报 device_id 可换键绕过
		// device 限流，IP 是唯一不可伪造的公共防线；置于验签前防 CPU 滥用。
		const ip = request.headers.get("CF-Connecting-IP");
		const ipLimit = rateLimit(
			this.ctx.storage.sql,
			`ip:${ip ?? "unknown"}`,
			"create-ip",
			this.L.createRateLimitPerMin,
			now,
		);
		if (!ipLimit.allowed) return ERR.rateLimited(ipLimit.retryAfter);
		// device 限流保留（自报键），但不再作为唯一防线（H1）。
		const devLimit = rateLimit(
			this.ctx.storage.sql,
			`device:${body.device_id}`,
			"create",
			this.L.createRateLimitPerMin,
			now,
		);
		if (!devLimit.allowed) return ERR.rateLimited(devLimit.retryAfter);
		// H1(b)：以 DTO 内 sender_ed_pub + Authorization 头验签（create 由
		// sender 自证身份）；失败 401，不产生 Channel DO。
		const auth = await verifyPeriSig(
			body.sender_ed_pub,
			request.headers.get("Authorization"),
			"create",
			[body.channel_id, body.device_id, body.expected_device_id, body.sender_ed_pub, body.sender_x_pub],
			now,
			this.L.signatureSkewSecs,
		);
		if (!auth.ok) return ERR.invalidSignature();
		if (auth.deviceId !== body.device_id) return ERR.invalidSignature();

		// 全部前置防线通过后才分配 Channel DO stub 并转发。
		const stub = (this.env as CodeIndexEnv).CHANNEL.get(
			(this.env as CodeIndexEnv).CHANNEL.idFromName(`v1:${body.channel_id}`),
		);
		const forwarded = new Request(request.url, {
			method: request.method,
			headers: request.headers,
			body: JSON.stringify(body),
		});
		const resp = await stub.fetch(forwarded);
		await channelHash(body.channel_id).then((h) => safeLog("create", resp.status === 201 || resp.status === 200 ? "ok" : "denied", { channelHash: h }));
		return resp;
	}

	// ─── lookup ─────────────────────────────────────────────────────────────

	private handleLookup(rawCode: string, now: number, ip: string | null): Response {
		const r = rateLimit(
			this.ctx.storage.sql,
			`ip:${ip ?? "unknown"}`,
			"lookup",
			this.L.codeLookupRateLimitPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);
		const code = normalizeSyncCode(rawCode);
		if (!code) return ERR.notFound(); // 非法码与 miss 统一 404（无 oracle）
		const rows = this.ctx.storage.sql
			.exec<CodeRow>("SELECT code, channel_id, expires_at FROM codes WHERE code = ?", code)
			.toArray();
		const row = rows[0] ?? null;
		if (!row) return ERR.notFound();
		if (row.expires_at <= now) {
			this.ctx.storage.sql.exec("DELETE FROM codes WHERE code = ?", code);
			return ERR.notFound();
		}
		safeLog("lookup", "ok", { codeShard: code[0] });
		return json({ channel_id: row.channel_id, valid_until: row.expires_at });
	}

	// ─── internal: register ─────────────────────────────────────────────────

	private async handleRegister(request: Request, now: number): Promise<Response> {
		let body: RegisterCodeInternal;
		try {
			body = (await request.json()) as RegisterCodeInternal;
		} catch {
			return ERR.badRequest();
		}
		if (typeof body.code !== "string" || typeof body.channel_id !== "string") {
			return ERR.badRequest();
		}
		const code = normalizeSyncCode(body.code);
		if (!code) return ERR.badRequest();
		this.purgeExpired(now);
		const rows = this.ctx.storage.sql
			.exec<CodeRow>("SELECT code, channel_id, expires_at FROM codes WHERE code = ?", code)
			.toArray();
		const row = rows[0] ?? null;
		if (row) {
			if (row.channel_id === body.channel_id) {
				// 同 channel 重放：幂等 200，不刷新有效期。
				return json({ expires_at: row.expires_at });
			}
			return ERR.collision(); // 40-bit 撞码
		}
		const expiresAt = now + this.L.codeValidSecs;
		this.ctx.storage.sql.exec(
			"INSERT INTO codes (code, channel_id, expires_at) VALUES (?, ?, ?)",
			code,
			body.channel_id,
			expiresAt,
		);
		safeLog("code", "ok", { codeShard: code[0] });
		return json({ expires_at: expiresAt });
	}

	// ─── internal: verify（join 时校验） ────────────────────────────────────

	private async handleVerify(request: Request, now: number): Promise<Response> {
		let body: VerifyCodeInternal;
		try {
			body = (await request.json()) as VerifyCodeInternal;
		} catch {
			return json({ valid: false } satisfies VerifyCodeInternalResponse);
		}
		const code = normalizeSyncCode(body.code);
		if (!code) return json({ valid: false } satisfies VerifyCodeInternalResponse);
		this.purgeExpired(now);
		const rows = this.ctx.storage.sql
			.exec<CodeRow>("SELECT code, channel_id, expires_at FROM codes WHERE code = ?", code)
			.toArray();
		const row = rows[0] ?? null;
		const valid =
			row !== null && row.channel_id === body.channel_id && row.expires_at > now;
		return json({ valid } satisfies VerifyCodeInternalResponse);
	}

	// ─── internal: delete（join/revoke 后全部码行失效） ────────────────────

	private async handleDelete(request: Request): Promise<Response> {
		try {
			const body = (await request.json()) as { channel_id: string };
			this.ctx.storage.sql.exec(
				"DELETE FROM codes WHERE channel_id = ?",
				body.channel_id,
			);
			return new Response(null, { status: 204 });
		} catch {
			return ERR.badRequest();
		}
	}

	private purgeExpired(now: number): void {
		this.ctx.storage.sql.exec("DELETE FROM codes WHERE expires_at < ?", now);
	}
}
