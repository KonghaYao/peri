/// <reference types="@cloudflare/workers-types" />
// Channel DO（r2-encrypted-transfer v1）——channel 状态事实源。
//
// SQLite 状态机：created → paired → ready → transferring → 终态
// （confirmed / revoked / expired）。TTL：created 600s / paired 300s /
// ready·transferring 3600s / 终态 tombstone 3600s，每次状态转移重设 alarm。
// R2 只存密文（channels/<channel_id>/parts/<idx>），写序固定为 R2 PUT 成功
// 后才写 manifest。
//
// alarm（L3）：活跃 TTL 到期先置终态（expired）再 best-effort 清理 R2
// （cleanupR2 try/catch，失败仅记日志不影响状态），孤儿对象由终态 tombstone
// 轮次重试清理；终态 tombstone 到期 deleteAll() 释放 DO+SQLite（M1），
// 此后 getChannel() 返回 null，一切 fetch 404（绝不 500）。
//
// 安全：验签密钥一律来自本 DO 登记记录（sender_ed_pub / expected_ed_pub /
// receiver_ed_pub），请求自报公钥只作逐字节比对，绝不用于验签（H1）。
// join 的身份比对先于限流，限流键用登记值 expected_device_id（M2）。

import { DurableObject } from "cloudflare:workers";
import {
	b64urlDecode,
	b64urlEncode,
	isValidChannelId,
	isValidDeviceId,
	isValidPubKey,
	normalizeSyncCode,
} from "./canonical";
import { sha256B64url, verifyPeriSig } from "./crypto";
import { createRateTables, rateLimit } from "./rate";
import { limits, type Limits } from "./limits";
import { channelHash, safeLog } from "./log";
import {
	ERR,
	NO_CONTENT,
	json,
	type ChannelState,
	type ChannelStateResponse,
	type CreateChannelRequest,
	type CreateChannelResponse,
	type HandshakeRequest,
	type HandshakeResponse,
	type HandshakeRole,
	type JoinChannelRequest,
	type RegisterCodeRequest,
	type UploadPartRequest,
	type UploadPartResponse,
} from "./v1-types";

export interface ChannelEnv {
	CHANNEL: DurableObjectNamespace;
	CODE_INDEX: DurableObjectNamespace;
	PERI_SYNC_PAYLOADS: R2Bucket;
	[key: string]: unknown;
}

interface ChannelRow extends Record<string, SqlStorageValue> {
	channel_id: string;
	state: ChannelState;
	sender_device_id: string;
	sender_ed_pub: string;
	sender_x_pub: string;
	expected_device_id: string;
	expected_ed_pub: string;
	receiver_device_id: string | null;
	receiver_ed_pub: string | null;
	receiver_x_pub: string | null;
	expires_at: number;
	tombstone_at: number | null;
	part_count: number;
	total_bytes: number;
}

interface HandshakeRow extends Record<string, SqlStorageValue> {
	seq: number;
	role: string;
	payload: ArrayBuffer;
	sha256: string;
}

interface PartRow extends Record<string, SqlStorageValue> {
	idx: number;
	size: number;
	sha256: string;
}

const ACTIVE_STATES: readonly ChannelState[] = ["created", "paired", "ready", "transferring"];
const TERMINAL_STATES: readonly ChannelState[] = ["confirmed", "revoked", "expired"];
const SIGNED_ROUTES = ["join", "code", "handshake", "upload", "download", "confirm", "revoke"];

export class Channel extends DurableObject {
	private readonly L: Limits;
	private readonly init: Promise<void>;

	constructor(ctx: DurableObjectState, env: ChannelEnv) {
		super(ctx, env);
		this.L = limits(env);
		this.init = ctx.blockConcurrencyWhile(async () => {
			this.ensureSchema();
		});
	}

	/** 建表（幂等）。deleteAll 后同实例重放 create 时再次调用（M1）。 */
	private ensureSchema(): void {
		this.ctx.storage.sql.exec(
			`CREATE TABLE IF NOT EXISTS channels (
				channel_id TEXT PRIMARY KEY,
				state TEXT NOT NULL,
				sender_device_id TEXT NOT NULL,
				sender_ed_pub TEXT NOT NULL,
				sender_x_pub TEXT NOT NULL,
				expected_device_id TEXT NOT NULL,
				expected_ed_pub TEXT NOT NULL,
				receiver_device_id TEXT,
				receiver_ed_pub TEXT,
				receiver_x_pub TEXT,
				expires_at INTEGER NOT NULL,
				tombstone_at INTEGER,
				part_count INTEGER NOT NULL DEFAULT 0,
				total_bytes INTEGER NOT NULL DEFAULT 0
			)`,
		);
		this.ctx.storage.sql.exec(
			`CREATE TABLE IF NOT EXISTS handshake (
				seq INTEGER PRIMARY KEY,
				role TEXT NOT NULL,
				payload BLOB NOT NULL,
				sha256 TEXT NOT NULL
			)`,
		);
		this.ctx.storage.sql.exec(
			`CREATE TABLE IF NOT EXISTS parts (
				idx INTEGER PRIMARY KEY,
				size INTEGER NOT NULL,
				sha256 TEXT NOT NULL
			)`,
		);
		createRateTables(this.ctx.storage.sql, SIGNED_ROUTES);
	}

	async fetch(request: Request): Promise<Response> {
		await this.init;
		const url = new URL(request.url);
		const now = Math.floor(Date.now() / 1000);

		if (request.method === "POST" && url.pathname === "/v1/channels") {
			return this.create(request, now);
		}
		if (request.method === "POST" && url.pathname.endsWith("/join")) {
			return this.join(request, now);
		}
		if (request.method === "POST" && url.pathname.endsWith("/code")) {
			return this.registerCode(request, now);
		}
		const hsMatch = url.pathname.match(/^\/v1\/channels\/[^/]+\/handshake\/(sender|receiver)$/);
		if (request.method === "POST" && hsMatch) {
			return this.handshake(request, hsMatch[1] as HandshakeRole, now);
		}
		if (request.method === "POST" && url.pathname.endsWith("/parts")) {
			return this.upload(request, now);
		}
		const dlMatch = url.pathname.match(/^\/v1\/channels\/[^/]+\/parts\/(\d+)$/);
		if (request.method === "GET" && dlMatch) {
			return this.download(dlMatch[1], request, now);
		}
		if (request.method === "POST" && url.pathname.endsWith("/confirm")) {
			return this.confirm(request, now);
		}
		if (request.method === "POST" && url.pathname.endsWith("/revoke")) {
			return this.revoke(request, now);
		}
		return ERR.notFound();
	}

	// ─── create ─────────────────────────────────────────────────────────────

	private async create(request: Request, now: number): Promise<Response> {
		let body: CreateChannelRequest;
		try {
			body = await request.json();
		} catch {
			return ERR.badRequest();
		}
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
		if (!isValidChannelId(body.channel_id) || !isValidDeviceId(body.device_id) || !isValidDeviceId(body.expected_device_id)) {
			return ERR.badRequest();
		}
		if (!isValidPubKey(body.expected_ed_pub) || !isValidPubKey(body.sender_ed_pub) || !isValidPubKey(body.sender_x_pub)) {
			return ERR.badRequest();
		}
		// create 验签密钥 = DTO 内 sender_ed_pub（sender 自证身份）。
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

		const existing = this.getChannel();
		if (existing) {
			const same =
				existing.sender_device_id === body.device_id &&
				existing.expected_device_id === body.expected_device_id &&
				existing.sender_ed_pub === body.sender_ed_pub &&
				existing.sender_x_pub === body.sender_x_pub &&
				existing.expected_ed_pub === body.expected_ed_pub;
			if (same) {
				const resp: CreateChannelResponse = { channel_id: body.channel_id, expires_at: existing.expires_at };
				return json(resp);
			}
			return ERR.conflict();
		}

		const expiresAt = now + this.L.createdSecs;
		// M1：终态 tombstone 到期 deleteAll 后，同实例重放 create 需先重建表。
		this.ensureSchema();
		this.ctx.storage.sql.exec(
			`INSERT INTO channels (
				channel_id, state, sender_device_id, sender_ed_pub, sender_x_pub,
				expected_device_id, expected_ed_pub, expires_at
			) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
			body.channel_id,
			"created",
			body.device_id,
			body.sender_ed_pub,
			body.sender_x_pub,
			body.expected_device_id,
			body.expected_ed_pub,
			expiresAt,
		);
		await this.setAlarmAt(expiresAt);
		const resp: CreateChannelResponse = { channel_id: body.channel_id, expires_at: expiresAt };
		return json(resp, 201);
	}

	// ─── join ───────────────────────────────────────────────────────────────

	private async join(request: Request, now: number): Promise<Response> {
		let body: JoinChannelRequest;
		try {
			body = await request.json();
		} catch {
			return ERR.badRequest();
		}
		if (
			typeof body.code !== "string" ||
			typeof body.device_id !== "string" ||
			typeof body.ed_pub !== "string" ||
			typeof body.x_pub !== "string"
		) {
			return ERR.badRequest();
		}
		if (!isValidDeviceId(body.device_id) || !isValidPubKey(body.ed_pub) || !isValidPubKey(body.x_pub)) {
			return ERR.badRequest();
		}
		const code = normalizeSyncCode(body.code);
		if (!code) return ERR.forbidden(); // 码窗/码格式不符

		const ch = this.getChannel();
		if (!ch) return ERR.notFound();
		if (TERMINAL_STATES.includes(ch.state)) return ERR.notFound();

		// M2：身份比对先于限流（防非预期设备填充限流桶 / oracle）；请求自报
		// 公钥/ID 必须与登记值逐字节相等，不等即 403（绝不用于验签）。
		// create 登记了 expected_device_id/expected_ed_pub；x_pub 无登记值，
		// 首次 join 写入，此后幂等 join 与已存值比对。
		if (
			body.device_id !== ch.expected_device_id ||
			body.ed_pub !== ch.expected_ed_pub ||
			(ch.receiver_x_pub !== null && body.x_pub !== ch.receiver_x_pub)
		) {
			return ERR.forbidden();
		}

		// 限流键用登记值 expected_device_id（不可伪造），而非自报 device_id。
		const r = rateLimit(
			this.ctx.storage.sql,
			`device:${ch.expected_device_id}`,
			"join",
			this.L.signedEndpointRateLimitPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);

		const auth = await verifyPeriSig(
			ch.expected_ed_pub,
			request.headers.get("Authorization"),
			"join",
			[ch.channel_id, code, body.device_id, body.ed_pub, body.x_pub],
			now,
			this.L.signatureSkewSecs,
		);
		if (!auth.ok) return ERR.invalidSignature();

		if (ch.state === "created") {
			const ok = await this.codeIndex().verifyCode(code, ch.channel_id, now);
			if (!ok) return ERR.forbidden(); // 码无效/过期/属于其他 channel
			const expiresAt = now + this.L.pairedSecs;
			this.ctx.storage.sql.exec(
				`UPDATE channels SET state = 'paired', receiver_device_id = ?, receiver_ed_pub = ?,
				 receiver_x_pub = ?, expires_at = ? WHERE channel_id = ?`,
				body.device_id,
				body.ed_pub,
				body.x_pub,
				expiresAt,
				ch.channel_id,
			);
			// join 成功即全部码行失效；跨 DO 删除失败由 60s TTL 自愈（best-effort）。
			await this.codeIndex().deleteCodes(ch.channel_id).catch(() => undefined);
			await this.setAlarmAt(expiresAt);
			safeLog("join", "ok", { channelHash: await channelHash(ch.channel_id) });
			return json({ state: "paired", expires_at: expiresAt } satisfies ChannelStateResponse, 201);
		}

		// 已 joined：同 receiver 幂等 200（跳过码查验，L2 固化）。
		if (ch.receiver_device_id === body.device_id) {
			return json({ state: ch.state, expires_at: ch.expires_at } satisfies ChannelStateResponse);
		}
		return ERR.forbidden(); // 异设备抢占
	}

	// ─── register code ──────────────────────────────────────────────────────

	private async registerCode(request: Request, now: number): Promise<Response> {
		let body: RegisterCodeRequest;
		try {
			body = await request.json();
		} catch {
			return ERR.badRequest();
		}
		if (typeof body.code !== "string" || typeof body.epoch !== "number") {
			return ERR.badRequest();
		}
		if (!Number.isInteger(body.epoch) || body.epoch < 0) return ERR.badRequest();
		const code = normalizeSyncCode(body.code);
		if (!code) return ERR.forbidden();

		const ch = this.getChannel();
		if (!ch) return ERR.notFound();
		if (TERMINAL_STATES.includes(ch.state)) return ERR.notFound();

		const r = rateLimit(
			this.ctx.storage.sql,
			`device:${ch.sender_device_id}`,
			"code",
			this.L.codeRegisterMaxPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);

		const auth = await verifyPeriSig(
			ch.sender_ed_pub,
			request.headers.get("Authorization"),
			"code",
			// I4：sha256 按归一化后码计算（上方 normalizeSyncCode 的结果），
			// 行为不变；Slice 3 Rust 客户端 hash 的也是归一化形式，两侧一致。
			[ch.channel_id, String(body.epoch), await sha256B64url(new TextEncoder().encode(code))],
			now,
			this.L.signatureSkewSecs,
		);
		if (!auth.ok) return ERR.invalidSignature();
		if (auth.deviceId !== ch.sender_device_id) return ERR.forbidden();

		// joined 后码使命结束（D4 固化）：仅 created 可注册。
		if (ch.state !== "created") return ERR.forbidden();

		return this.codeIndex().registerCode(code, ch.channel_id, now);
	}

	// ─── handshake ──────────────────────────────────────────────────────────

	private async handshake(request: Request, role: HandshakeRole, now: number): Promise<Response> {
		let body: HandshakeRequest;
		try {
			body = await request.json();
		} catch {
			return ERR.badRequest();
		}
		if (body.msg !== undefined && body.msg !== null && typeof body.msg !== "string") {
			return ERR.badRequest();
		}
		const payload =
			body.msg === undefined || body.msg === null
				? new Uint8Array(0)
				: b64urlDecode(body.msg);
		if (payload === null) return ERR.badRequest();
		if (payload.length > this.L.maxMsgBytes) return ERR.tooLarge();
		const seq = role === "sender" ? 1 : 2;

		const ch = this.getChannel();
		if (!ch) return ERR.notFound();
		if (TERMINAL_STATES.includes(ch.state)) return ERR.notFound();

		const r = rateLimit(
			this.ctx.storage.sql,
			`device:${role === "sender" ? ch.sender_device_id : ch.receiver_device_id ?? ""}`,
			"handshake",
			this.L.signedEndpointRateLimitPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);

		const payloadHash = await sha256B64url(payload);
		const seqStr = String(seq);
		if (role === "sender") {
			if (ch.state === "confirmed" || ch.state === "revoked" || ch.state === "expired") {
				return ERR.notFound();
			}
			const auth = await verifyPeriSig(
				ch.sender_ed_pub,
				request.headers.get("Authorization"),
				"msg",
				[ch.channel_id, seqStr, payloadHash],
				now,
				this.L.signatureSkewSecs,
			);
			if (!auth.ok) return ERR.invalidSignature();
			if (auth.deviceId !== ch.sender_device_id) return ERR.forbidden();
		} else {
			if (ch.state === "created" || ch.state === "confirmed" || ch.state === "revoked" || ch.state === "expired") {
				// msg2 仅 receiver：需已 join（created → 403；终态 → 404）
				return ch.state === "created" ? ERR.forbidden() : ERR.notFound();
			}
			if (!ch.receiver_ed_pub) return ERR.forbidden();
			const auth = await verifyPeriSig(
				ch.receiver_ed_pub,
				request.headers.get("Authorization"),
				"msg",
				[ch.channel_id, seqStr, payloadHash],
				now,
				this.L.signatureSkewSecs,
			);
			if (!auth.ok) return ERR.invalidSignature();
			if (auth.deviceId !== ch.receiver_device_id) return ERR.forbidden();
			// 契约：seq2 需 seq1 已存在。
			if (payload.length > 0 && !this.getHandshake(1)) return ERR.forbidden();
		}

		const existing = this.getHandshake(seq);
		if (existing) {
			if (existing.sha256 !== payloadHash) return ERR.conflict(); // 同 seq 异 payload
			// 幂等：不重写
		} else if (payload.length > 0) {
			this.ctx.storage.sql.exec(
				"INSERT INTO handshake (seq, role, payload, sha256) VALUES (?, ?, ?, ?)",
				seq,
				role,
				payload,
				payloadHash,
			);
		}

		// 两条消息齐全 → ready（TTL 3600，重设 alarm）。
		let state = ch.state;
		let expiresAt = ch.expires_at;
		const both = this.getHandshake(1) !== null && this.getHandshake(2) !== null;
		if (both && state === "paired") {
			state = "ready";
			expiresAt = now + this.L.readySecs;
			this.ctx.storage.sql.exec(
				"UPDATE channels SET state = 'ready', expires_at = ? WHERE channel_id = ?",
				expiresAt,
				ch.channel_id,
			);
			await this.setAlarmAt(expiresAt);
			safeLog("ready", "ok", { channelHash: await channelHash(ch.channel_id) });
		}

		const peer = seq === 1 ? this.getHandshake(2) : this.getHandshake(1);
		const resp: HandshakeResponse = {
			peer_msg: peer ? b64urlEncode(new Uint8Array(peer.payload)) : null,
			state,
			expires_at: expiresAt,
		};
		return json(resp);
	}

	// ─── upload ─────────────────────────────────────────────────────────────

	private async upload(request: Request, now: number): Promise<Response> {
		let body: UploadPartRequest;
		try {
			body = await request.json();
		} catch {
			return ERR.badRequest();
		}
		if (typeof body.part_index !== "number" || typeof body.ciphertext !== "string") {
			return ERR.badRequest();
		}
		if (!Number.isInteger(body.part_index) || body.part_index < 0) return ERR.badRequest();
		const ct = b64urlDecode(body.ciphertext);
		if (ct === null) return ERR.badRequest();
		if (ct.length > this.L.maxPartBytes) return ERR.tooLarge();

		const ch = this.getChannel();
		if (!ch) return ERR.notFound();
		if (TERMINAL_STATES.includes(ch.state)) return ERR.notFound();

		const r = rateLimit(
			this.ctx.storage.sql,
			`device:${ch.sender_device_id}`,
			"upload",
			this.L.signedEndpointRateLimitPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);

		const ctHash = await sha256B64url(ct);
		const auth = await verifyPeriSig(
			ch.sender_ed_pub,
			request.headers.get("Authorization"),
			"upload",
			[ch.channel_id, String(body.part_index), ctHash],
			now,
			this.L.signatureSkewSecs,
		);
		if (!auth.ok) return ERR.invalidSignature();
		if (auth.deviceId !== ch.sender_device_id) return ERR.forbidden();

		// 仅 ready/transferring 可上传。
		if (ch.state !== "ready" && ch.state !== "transferring") return ERR.forbidden();

		if (body.part_index >= this.L.maxPartsPerChannel) return ERR.tooLarge();

		const existing = this.getPart(body.part_index);
		if (existing) {
			if (existing.sha256 === ctHash) {
				const resp: UploadPartResponse = { part_index: body.part_index, size: existing.size };
				return json(resp); // 幂等：不重写 R2
			}
			return ERR.conflict(); // 同 idx 异内容
		}

		// manifest 预算先行校验（413 且不写 R2）。
		if (ch.part_count + 1 > this.L.maxPartsPerChannel) return ERR.tooLarge();
		if (ch.total_bytes + ct.length > this.L.maxPayloadBytes) return ERR.tooLarge();

		// 写序：R2 PUT 成功 → manifest 落库。
		const key = `channels/${ch.channel_id}/parts/${body.part_index}`;
		try {
			await this.r2().put(key, ct, { httpMetadata: { contentType: "application/octet-stream" } });
		} catch {
			safeLog("upload", "error", { channelHash: await channelHash(ch.channel_id) });
			return new Response("storage unavailable", { status: 503 });
		}

		this.ctx.storage.sql.exec(
			"INSERT INTO parts (idx, size, sha256) VALUES (?, ?, ?)",
			body.part_index,
			ct.length,
			ctHash,
		);
		let state = ch.state;
		let expiresAt = ch.expires_at;
		if (state === "ready") {
			state = "transferring";
			expiresAt = now + this.L.readySecs;
		}
		this.ctx.storage.sql.exec(
			"UPDATE channels SET state = ?, part_count = ?, total_bytes = ?, expires_at = ? WHERE channel_id = ?",
			state,
			ch.part_count + 1,
			ch.total_bytes + ct.length,
			expiresAt,
			ch.channel_id,
		);
		await this.setAlarmAt(expiresAt);
		const resp: UploadPartResponse = { part_index: body.part_index, size: ct.length };
		return json(resp, 201);
	}

	// ─── download ───────────────────────────────────────────────────────────

	private async download(idxStr: string, request: Request, now: number): Promise<Response> {
		const idx = parseInt(idxStr, 10);
		if (!Number.isInteger(idx) || idx < 0) return ERR.badRequest();

		const ch = this.getChannel();
		if (!ch) return ERR.notFound();
		if (TERMINAL_STATES.includes(ch.state)) return ERR.notFound();

		const r = rateLimit(
			this.ctx.storage.sql,
			`device:${ch.receiver_device_id ?? ""}`,
			"download",
			this.L.signedEndpointRateLimitPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);

		if (!ch.receiver_ed_pub) return ERR.forbidden();
		const auth = await verifyPeriSig(
			ch.receiver_ed_pub,
			request.headers.get("Authorization"),
			"download",
			[ch.channel_id, idxStr],
			now,
			this.L.signatureSkewSecs,
		);
		if (!auth.ok) return ERR.invalidSignature();
		if (auth.deviceId !== ch.receiver_device_id) return ERR.forbidden();

		// 仅 ready/transferring 可下载。
		if (ch.state !== "ready" && ch.state !== "transferring") return ERR.forbidden();

		const part = this.getPart(idx);
		if (!part) return ERR.notFound(); // 缺 part：可重试

		const key = `channels/${ch.channel_id}/parts/${idx}`;
		const range = request.headers.get("Range");
		let obj: R2ObjectBody | null;
		try {
			if (range) {
				const rh = new Headers();
				rh.set("Range", range);
				obj = await this.r2().get(key, { range: rh });
			} else {
				obj = await this.r2().get(key);
			}
		} catch {
			safeLog("download", "error", { channelHash: await channelHash(ch.channel_id) });
			return new Response("storage unavailable", { status: 503 });
		}
		if (obj === null) return ERR.notFound();
		const headers = new Headers();
		headers.set("content-type", "application/octet-stream");
		if (obj.etag) headers.set("etag", obj.etag);
		// 仅客户端带 Range 头时回 206（miniflare 模拟对普通 get 也返回 range 元数据，
		// 不能以 obj.range 判定）。
		if (range && obj.range) {
			headers.set("content-range", contentRangeHeader(obj));
			return new Response(obj.body, { status: 206, headers });
		}
		return new Response(obj.body, { status: 200, headers });
	}

	// ─── confirm ────────────────────────────────────────────────────────────

	private async confirm(request: Request, now: number): Promise<Response> {
		const ch = this.getChannel();
		if (!ch) return ERR.notFound();
		if (TERMINAL_STATES.includes(ch.state) && ch.state !== "confirmed") return ERR.notFound();

		const r = rateLimit(
			this.ctx.storage.sql,
			`device:${ch.receiver_device_id ?? ""}`,
			"confirm",
			this.L.signedEndpointRateLimitPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);

		if (!ch.receiver_ed_pub) return ERR.forbidden();
		const auth = await verifyPeriSig(
			ch.receiver_ed_pub,
			request.headers.get("Authorization"),
			"confirm",
			// I3：冻结字段序 = [channel_id, ts]（03-plan "签名字段序冻结"）。
			[ch.channel_id],
			now,
			this.L.signatureSkewSecs,
		);
		if (!auth.ok) return ERR.invalidSignature();
		if (auth.deviceId !== ch.receiver_device_id) return ERR.forbidden();

		if (ch.state === "confirmed") return NO_CONTENT; // 同 receiver 幂等

		if (ch.state !== "ready" && ch.state !== "transferring") return ERR.forbidden();

		const tombstoneAt = now + this.L.tombstoneSecs;
		this.ctx.storage.sql.exec(
			"UPDATE channels SET state = 'confirmed', expires_at = ?, tombstone_at = ? WHERE channel_id = ?",
			tombstoneAt,
			tombstoneAt,
			ch.channel_id,
		);
		await this.setAlarmAt(tombstoneAt);
		safeLog("confirm", "ok", { channelHash: await channelHash(ch.channel_id) });
		return NO_CONTENT;
	}

	// ─── revoke ─────────────────────────────────────────────────────────────

	private async revoke(request: Request, now: number): Promise<Response> {
		const ch = this.getChannel();
		if (!ch) return ERR.notFound();
		if (ch.state === "expired") return ERR.notFound();

		const r = rateLimit(
			this.ctx.storage.sql,
			`device:${ch.sender_device_id}`,
			"revoke",
			this.L.signedEndpointRateLimitPerMin,
			now,
		);
		if (!r.allowed) return ERR.rateLimited(r.retryAfter);

		const auth = await verifyPeriSig(
			ch.sender_ed_pub,
			request.headers.get("Authorization"),
			"revoke",
			// I3：冻结字段序 = [channel_id, ts]（03-plan "签名字段序冻结"）。
			[ch.channel_id],
			now,
			this.L.signatureSkewSecs,
		);
		if (!auth.ok) return ERR.invalidSignature();
		if (auth.deviceId !== ch.sender_device_id) return ERR.forbidden();

		if (ch.state === "revoked") return NO_CONTENT; // 同 sender 幂等
		if (ch.state === "confirmed") return ERR.notFound(); // 终态不可逆

		const tombstoneAt = now + this.L.tombstoneSecs;
		this.ctx.storage.sql.exec(
			"UPDATE channels SET state = 'revoked', expires_at = ?, tombstone_at = ? WHERE channel_id = ?",
			tombstoneAt,
			tombstoneAt,
			ch.channel_id,
		);
		await this.codeIndex().deleteCodes(ch.channel_id).catch(() => undefined); // 撤销即码失效（D3 固化）；失败由 TTL 自愈
		await this.setAlarmAt(tombstoneAt);
		safeLog("revoke", "ok", { channelHash: await channelHash(ch.channel_id) });
		return NO_CONTENT;
	}

	// ─── alarm：活跃到期先置终态再 best-effort 清 R2；终态 deleteAll ────────

	async alarm(): Promise<void> {
		await this.init;
		const now = Math.floor(Date.now() / 1000);
		const ch = this.getChannel();
		if (!ch) {
			await this.ctx.storage.deleteAlarm();
			return;
		}
		if (now < ch.expires_at) {
			// 提前触发（理论上不应发生）：重排到期限。
			await this.ctx.storage.setAlarm(ch.expires_at * 1000);
			return;
		}
		// 清理过期 rate 行（活跃状态路径；终态 deleteAll 会清除全部表）。
		for (const route of SIGNED_ROUTES) {
			this.ctx.storage.sql.exec(`DELETE FROM rate_${route} WHERE window_start < ?`, now - 3600);
		}
		if (ACTIVE_STATES.includes(ch.state)) {
			// L3：状态先置终态（状态机正确性优先），R2 清理为 best-effort；
			// 失败留下的孤儿对象由终态 tombstone 轮次重试清理。
			const tombstoneAt = now + this.L.tombstoneSecs;
			this.ctx.storage.sql.exec(
				"UPDATE channels SET state = 'expired', expires_at = ?, tombstone_at = ? WHERE channel_id = ?",
				tombstoneAt,
				tombstoneAt,
				ch.channel_id,
			);
			await this.cleanupR2Safe(ch.channel_id);
			safeLog("expire", "ok", { channelHash: await channelHash(ch.channel_id) });
			await this.ctx.storage.setAlarm(tombstoneAt * 1000);
			return;
		}
		// 终态 tombstone 到期：重试 R2 孤儿清理 → deleteAll 释放 DO+SQLite
		// （M1）。deleteAll 后 getChannel() 返回 null，一切 fetch 404。
		await this.cleanupR2Safe(ch.channel_id);
		safeLog("cleanup", "ok", { channelHash: await channelHash(ch.channel_id) });
		await this.ctx.storage.deleteAll();
	}

	// ─── 内部辅助 ───────────────────────────────────────────────────────────

	private getChannel(): ChannelRow | null {
		try {
			const rows = this.ctx.storage.sql
				.exec<ChannelRow>("SELECT * FROM channels LIMIT 1")
				.toArray();
			return rows[0] ?? null;
		} catch {
			// M1：deleteAll 后同实例 fetch（表已删）→ 视为未知 channel（404），
			// 绝不 500。
			return null;
		}
	}

	private getHandshake(seq: number): HandshakeRow | null {
		const rows = this.ctx.storage.sql
			.exec<HandshakeRow>("SELECT seq, role, payload, sha256 FROM handshake WHERE seq = ?", seq)
			.toArray();
		return rows[0] ?? null;
	}

	private getPart(idx: number): PartRow | null {
		const rows = this.ctx.storage.sql
			.exec<PartRow>("SELECT idx, size, sha256 FROM parts WHERE idx = ?", idx)
			.toArray();
		return rows[0] ?? null;
	}

	private async setAlarmAt(unixSecs: number): Promise<void> {
		await this.ctx.storage.setAlarm(unixSecs * 1000);
	}

	/** best-effort R2 prefix 清理（L3）：失败仅记日志，不影响状态转移。 */
	private async cleanupR2Safe(channelId: string): Promise<void> {
		try {
			await this.cleanupR2(channelId);
		} catch {
			safeLog("cleanup", "error");
		}
	}

	private async cleanupR2(channelId: string): Promise<void> {
		const prefix = `channels/${channelId}/parts/`;
		let cursor: string | undefined;
		do {
			const listed = await this.r2().list({ prefix, cursor });
			if (listed.objects.length === 0) break;
			await this.r2().delete(
				listed.objects.map((o) => o.key),
			);
			cursor = listed.truncated ? listed.cursor : undefined;
		} while (cursor);
	}

	private codeIndex() {
		const env = this.env as ChannelEnv;
		const ns = env.CODE_INDEX;
		const stub = ns.get(ns.idFromName("v1:index"));
		return {
			registerCode: (code: string, channelId: string, now: number): Promise<Response> =>
				stub.fetch("https://do.internal/internal/register", {
					method: "POST",
					body: JSON.stringify({ code, channel_id: channelId, now }),
				}),
			verifyCode: async (code: string, channelId: string, now: number): Promise<boolean> => {
				const resp = await stub.fetch("https://do.internal/internal/verify", {
					method: "POST",
					body: JSON.stringify({ code, channel_id: channelId, now }),
				});
				if (!resp.ok) return false;
				const out = (await resp.json()) as { valid: boolean };
				return out.valid === true;
			},
			deleteCodes: (channelId: string): Promise<Response> =>
				stub.fetch("https://do.internal/internal/delete", {
					method: "POST",
					body: JSON.stringify({ channel_id: channelId }),
				}),
		};
	}

	private r2(): R2Bucket {
		return (this.env as ChannelEnv).PERI_SYNC_PAYLOADS;
	}
}

/** 由 R2 range 元数据构造 `content-range` 响应头（206）。 */
function contentRangeHeader(obj: R2ObjectBody): string {
	const r = obj.range as { offset?: number; length?: number; suffix?: number } | undefined;
	if (r && r.suffix !== undefined) {
		return `bytes ${obj.size - r.suffix}-${obj.size - 1}/${obj.size}`;
	}
	if (r && r.offset !== undefined && r.length !== undefined) {
		return `bytes ${r.offset}-${r.offset + r.length - 1}/${obj.size}`;
	}
	return `bytes */${obj.size}`;
}
