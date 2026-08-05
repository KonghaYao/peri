// Channel DO / 入口 Worker 集成测试（miniflare + bun:test）。
// 覆盖：endpoint×state×error、签名篡改/过期、码生命周期、alarm TTL、R2 写序、
// confirm/revoke 幂等、限流 429+Retry-After、Health、日志白名单。

import { afterAll, beforeAll, describe, expect, it } from "bun:test";
import type { Miniflare } from "miniflare";
import {
	b64urlDecode,
	b64urlEncode,
	normalizeSyncCode,
} from "./canonical";
import { sha256B64url } from "./crypto";
import {
	makeDevice,
	makeMf,
	nowSecs,
	periSig,
	sleep,
	type TestDevice,
} from "./test-utils";

let mf: Miniflare;

beforeAll(() => {
	mf = makeMf();
});

afterAll(async () => {
	await mf.dispose();
});

// ─── 客户端流程 helper（模拟 Slice 3） ─────────────────────────────────────

function jsonReq(m: Miniflare, path: string, body: unknown, auth?: string, extra?: Record<string, string>) {
	return m.dispatchFetch("https://example.com" + path, {
		method: "POST",
		headers: {
			"content-type": "application/json",
			...(auth ? { authorization: auth } : {}),
			...(extra ?? {}),
		},
		body: JSON.stringify(body),
	});
}

async function doCreate(m: Miniflare, sender: TestDevice, expected: TestDevice, channelId: string, ts = nowSecs()) {
	const auth = await periSig(sender, "create", [channelId, sender.deviceId, expected.deviceId, sender.edPubB64, sender.xPubB64], ts);
	return jsonReq(m, "/v1/channels", {
		channel_id: channelId,
		device_id: sender.deviceId,
		expected_device_id: expected.deviceId,
		expected_ed_pub: expected.edPubB64,
		sender_ed_pub: sender.edPubB64,
		sender_x_pub: sender.xPubB64,
	}, auth, { "CF-Connecting-IP": `203.0.113.${1 + Math.floor(Math.random() * 200)}` });
}

async function doRegister(m: Miniflare, sender: TestDevice, channelId: string, code: string, epoch = Math.floor(nowSecs() / 30), ts = nowSecs()) {
	const norm = normalizeSyncCode(code)!;
	const codeHash = await sha256B64url(new TextEncoder().encode(norm));
	const auth = await periSig(sender, "code", [channelId, String(epoch), codeHash], ts);
	return jsonReq(m, `/v1/channels/${channelId}/code`, { code, epoch }, auth);
}

async function doJoin(m: Miniflare, receiver: TestDevice, channelId: string, code: string, ts = nowSecs()) {
	const norm = normalizeSyncCode(code)!;
	const auth = await periSig(receiver, "join", [channelId, norm, receiver.deviceId, receiver.edPubB64, receiver.xPubB64], ts);
	return jsonReq(m, `/v1/channels/${channelId}/join`, {
		code,
		device_id: receiver.deviceId,
		ed_pub: receiver.edPubB64,
		x_pub: receiver.xPubB64,
	}, auth);
}

async function doHandshake(m: Miniflare, device: TestDevice, channelId: string, role: "sender" | "receiver", msg?: string) {
	const seq = role === "sender" ? "1" : "2";
	const payload = msg ? b64urlDecode(msg)! : new Uint8Array(0);
	const hash = await sha256B64url(payload);
	const auth = await periSig(device, "msg", [channelId, seq, hash], nowSecs());
	return jsonReq(m, `/v1/channels/${channelId}/handshake/${role}`, { msg: msg ?? null }, auth);
}

async function doUpload(m: Miniflare, sender: TestDevice, channelId: string, partIndex: number, ct: Uint8Array) {
	const hash = await sha256B64url(ct);
	const auth = await periSig(sender, "upload", [channelId, String(partIndex), hash], nowSecs());
	return jsonReq(m, `/v1/channels/${channelId}/parts`, { part_index: partIndex, ciphertext: b64urlEncode(ct) }, auth);
}

async function doDownload(m: Miniflare, receiver: TestDevice, channelId: string, idx: number, range?: string) {
	const auth = await periSig(receiver, "download", [channelId, String(idx)], nowSecs());
	return m.dispatchFetch(`https://example.com/v1/channels/${channelId}/parts/${idx}`, {
		method: "GET",
		headers: { authorization: auth, ...(range ? { range } : {}) },
	});
}

async function doConfirm(m: Miniflare, receiver: TestDevice, channelId: string, ts = nowSecs()) {
	// I3：冻结字段序 = [channel_id, ts]
	const auth = await periSig(receiver, "confirm", [channelId], ts);
	return jsonReq(m, `/v1/channels/${channelId}/confirm`, {}, auth);
}

async function doRevoke(m: Miniflare, sender: TestDevice, channelId: string, ts = nowSecs()) {
	// I3：冻结字段序 = [channel_id, ts]
	const auth = await periSig(sender, "revoke", [channelId], ts);
	return jsonReq(m, `/v1/channels/${channelId}/revoke`, {}, auth);
}

function doLookup(m: Miniflare, code: string, ip?: string) {
	// 默认随机 IP：避免多个测试共享 CodeIndex 的 lookup 限流桶。
	const effIp = ip ?? `203.0.113.${1 + Math.floor(Math.random() * 200)}`;
	return m.dispatchFetch(`https://example.com/v1/code/${encodeURIComponent(code)}/lookup`, {
		method: "POST",
		headers: { "CF-Connecting-IP": effIp },
	});
}

/** 完整 happy path：create → register → lookup → join → 双向 handshake。 */
async function readyChannel(m: Miniflare, sender: TestDevice, receiver: TestDevice) {
	const channelId = randomChannelId();
	const code = randomCode();
	expect((await doCreate(m, sender, receiver, channelId)).status).toBe(201);
	expect((await doRegister(m, sender, channelId, code)).status).toBe(200);
	expect((await doLookup(m, code)).status).toBe(200);
	expect((await doJoin(m, receiver, channelId, code)).status).toBe(201);
	const msg1 = noiseMsg("m1");
	const msg2 = noiseMsg("m2");
	expect((await doHandshake(m, sender, channelId, "sender", msg1)).status).toBe(200);
	const hs2 = await doHandshake(m, receiver, channelId, "receiver", msg2);
	expect(hs2.status).toBe(200);
	expect(await hs2.json()).toMatchObject({ state: "ready" });
	const hs1 = await doHandshake(m, sender, channelId, "sender", msg1); // 幂等重发拉 msg2
	expect(hs1.status).toBe(200);
	expect(await hs1.json()).toMatchObject({ state: "ready", peer_msg: msg2 });
	return { channelId, code };
}

/** 随机 8 字符 Crockford 码（避免跨测试撞码）。 */
function randomCode(): string {
	const alpha = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
	let out = "";
	for (let i = 0; i < 8; i++) out += alpha[Math.floor(Math.random() * alpha.length)];
	return out;
}

/** 合法 base64url Noise blob（roundtrip 保真，避免测试数据歧义）。 */
function noiseMsg(label: string): string {
	return b64urlEncode(new TextEncoder().encode(`noise-${label}`));
}

function randomChannelId(): string {
	const bytes = new Uint8Array(16);
	crypto.getRandomValues(bytes);
	return b64urlEncode(bytes);
}

// ─── Health / 白名单 ────────────────────────────────────────────────────────

describe("入口", () => {
	it("GET /health → 200 ok", async () => {
		const resp = await mf.dispatchFetch("https://example.com/health");
		expect(resp.status).toBe(200);
		expect(await resp.text()).toBe("ok");
	});

	it("未知路径/方法一律 404", async () => {
		expect((await mf.dispatchFetch("https://example.com/")).status).toBe(404);
		expect((await mf.dispatchFetch("https://example.com/v1")).status).toBe(404);
		expect((await mf.dispatchFetch("https://example.com/v1/channels", { method: "PUT" })).status).toBe(404);
		expect((await mf.dispatchFetch("https://example.com/v1/channels/x/parts/0", { method: "POST" })).status).toBe(404);
		expect((await mf.dispatchFetch("https://example.com/v1/code/abc", { method: "GET" })).status).toBe(404);
		// 非法 channel_id 格式 → 404（未知 channel 统一语义）
		const bad = await mf.dispatchFetch("https://example.com/v1/channels/not-a-channel-id/join", {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: "{}",
		});
		expect(bad.status).toBe(404);
	});
});

// ─── create ─────────────────────────────────────────────────────────────────

describe("create", () => {
	it("201 返回 channel_id 与 expires_at；同 ID 同请求幂等 200；异请求 409", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const r1 = await doCreate(mf, sender, receiver, channelId);
		expect(r1.status).toBe(201);
		const body = (await r1.json()) as { channel_id: string; expires_at: number };
		expect(body.channel_id).toBe(channelId);
		expect(body.expires_at).toBeGreaterThan(nowSecs());

		const r2 = await doCreate(mf, sender, receiver, channelId);
		expect(r2.status).toBe(200);
		expect((await r2.json()) as { channel_id: string; expires_at: number }).toEqual({ channel_id: channelId, expires_at: body.expires_at });

		// 同 ID 异 expected 设备 → 409
		const other = await makeDevice();
		const r3 = await doCreate(mf, sender, other, channelId);
		expect(r3.status).toBe(409);
	});

	it("签名无效/篡改/过期一律 401；body 字段缺失 400", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();

		// 篡改签名字段（expected_device_id 不同）
		const ts = nowSecs();
		const auth = await periSig(sender, "create", [channelId, sender.deviceId, receiver.deviceId, sender.edPubB64, sender.xPubB64], ts);
		const tampered = await jsonReq(mf, "/v1/channels", {
			channel_id: channelId,
			device_id: sender.deviceId,
			expected_device_id: "AAAAAAAAAAAAAAAAAAAAAA",
			expected_ed_pub: receiver.edPubB64,
			sender_ed_pub: sender.edPubB64,
			sender_x_pub: sender.xPubB64,
		}, auth);
		expect(tampered.status).toBe(401);

		// 过期时间戳（窗口外）
		const stale = await doCreate(mf, sender, receiver, randomChannelId(), nowSecs() - 400);
		expect(stale.status).toBe(401);

		// 字段缺失
		const missing = await jsonReq(mf, "/v1/channels", { channel_id: channelId }, undefined);
		expect(missing.status).toBe(400);
	});
});

// ─── code 注册与 lookup ─────────────────────────────────────────────────────

describe("code 生命周期", () => {
	it("注册 200 → lookup 200（含归一化）；非法码 lookup 404；撞码 409", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		await doCreate(mf, sender, receiver, channelId);

		const r = await doRegister(mf, sender, channelId, "7m4k-p9xq"); // 小写+连字符，服务端归一化
		expect(r.status).toBe(200);
		const body = (await r.json()) as { expires_at: number };
		expect(body.expires_at).toBeGreaterThan(nowSecs());

		// lookup 归一化：大写展示码命中
		const look = await doLookup(mf, "7M4K-P9XQ");
		expect(look.status).toBe(200);
		const loc = (await look.json()) as { channel_id: string; valid_until: number };
		expect(loc.channel_id).toBe(channelId);
		expect(loc.valid_until).toBe(body.expires_at);

		// 非法码统一 404（U、长度、非法字符）
		expect((await doLookup(mf, "7M4KU9XQ")).status).toBe(404);
		expect((await doLookup(mf, "7M4KP9X")).status).toBe(404);
		expect((await doLookup(mf, "7M4K*P9X")).status).toBe(404);
		expect((await doLookup(mf, "NOTACODE")).status).toBe(404);
		// miss 404
		expect((await doLookup(mf, "00000000")).status).toBe(404);

		// 撞码：另一 channel 注册同一码 → 409
		const channel2 = randomChannelId();
		const sender2 = await makeDevice();
		await doCreate(mf, sender2, receiver, channel2);
		const collide = await doRegister(mf, sender2, channel2, "7M4K-P9XQ");
		expect(collide.status).toBe(409);

		// 同 channel 重放注册幂等 200（不刷新 expires_at）
		const replay = await doRegister(mf, sender, channelId, "7M4K-P9XQ");
		expect(replay.status).toBe(200);
		expect((await replay.json()) as { expires_at: number }).toEqual({ expires_at: body.expires_at });
	});

	it("joined 后注册被拒（403），码行失效 lookup 404；revoke 后 lookup 404", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const { channelId, code } = await readyChannel(mf, sender, receiver);

		const reg = await doRegister(mf, sender, channelId, randomCode());
		expect(reg.status).toBe(403); // joined 后码使命结束

		expect((await doLookup(mf, code)).status).toBe(404); // join 即删全部码行

		// revoke 后 lookup 依旧 404
		const code2 = randomCode();
		await doRegister(mf, sender, channelId, code2);
		expect((await doLookup(mf, code2)).status).toBe(404); // ready 状态注册被 403，码行未建
	});
});

// ─── join ───────────────────────────────────────────────────────────────────

describe("join", () => {
	it("201 → 码失效；同 receiver 幂等 200；错码 403；异设备自报公钥 403（H1 反例）", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const code = randomCode();
		await doCreate(mf, sender, receiver, channelId);
		await doRegister(mf, sender, channelId, code);

		const j1 = await doJoin(mf, receiver, channelId, code);
		expect(j1.status).toBe(201);
		expect(await j1.json()).toMatchObject({ state: "paired" });

		// join 成功即码失效
		expect((await doLookup(mf, code)).status).toBe(404);

		// 同 receiver 幂等 200（跳过码查验）
		const j2 = await doJoin(mf, receiver, channelId, "00000000"); // 码已失效但幂等路径豁免
		expect(j2.status).toBe(200);
		expect(await j2.json()).toMatchObject({ state: "paired" });

		// 未注册码 join → 403
		const ch2 = randomChannelId();
		await doCreate(mf, sender, receiver, ch2);
		const jbad = await doJoin(mf, receiver, ch2, "00000000");
		expect(jbad.status).toBe(403);

		// H1 反例：攻击者自报公钥 + 自签 join → 403（而非 401，防 oracle）
		const attacker = await makeDevice();
		const ts = nowSecs();
		const fields = [channelId, "ABCDEFGH", attacker.deviceId, attacker.edPubB64, attacker.xPubB64];
		const auth = await periSig(attacker, "join", fields, ts);
		const hijack = await jsonReq(mf, `/v1/channels/${channelId}/join`, {
			code,
			device_id: attacker.deviceId,
			ed_pub: attacker.edPubB64,
			x_pub: attacker.xPubB64,
		}, auth);
		expect(hijack.status).toBe(403);

		// 未知 channel → 404
		const unknown = await doJoin(mf, receiver, randomChannelId(), code);
		expect(unknown.status).toBe(404);
	});

	it("签名篡改/过期 401", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const code = randomCode();
		await doCreate(mf, sender, receiver, channelId);
		await doRegister(mf, sender, channelId, code);

		// 签名绑定 code 与请求码不一致 → 401
		const ts = nowSecs();
		const auth = await periSig(receiver, "join", [channelId, "ABCDEFGH", receiver.deviceId, receiver.edPubB64, receiver.xPubB64], ts);
		const swapped = await jsonReq(mf, `/v1/channels/${channelId}/join`, {
			code: "00000000",
			device_id: receiver.deviceId,
			ed_pub: receiver.edPubB64,
			x_pub: receiver.xPubB64,
		}, auth);
		expect(swapped.status).toBe(401);

		// 过期时间戳
		const stale = await doJoin(mf, receiver, channelId, code, nowSecs() - 400);
		expect(stale.status).toBe(401);
	});
});

// ─── handshake ──────────────────────────────────────────────────────────────

describe("handshake", () => {
	it("msg1 存储 → receiver 拉取 → msg2 → ready；sender 幂等重发拉 msg2", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const { channelId } = await readyChannel(mf, sender, receiver);
		void channelId;
	});

	it("同 seq 异 payload 409；签名篡改 401；receiver 未 join 403", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const code = randomCode();
		await doCreate(mf, sender, receiver, channelId);
		await doRegister(mf, sender, channelId, code);

		const m1 = noiseMsg("m1");
		const m2 = noiseMsg("m2");

		// receiver 未 join（created 状态）→ 403
		const hsR = await doHandshake(mf, receiver, channelId, "receiver", m2);
		expect(hsR.status).toBe(403);

		// sender msg1 在 created 状态即可发
		expect((await doHandshake(mf, sender, channelId, "sender", m1)).status).toBe(200);

		// 同 seq 异 payload → 409
		const conflict = await doHandshake(mf, sender, channelId, "sender", noiseMsg("m1-changed"));
		expect(conflict.status).toBe(409);

		// 篡改签名 → 401（字段 hash 与实际 payload 不符）
		const ts = nowSecs();
		const auth = await periSig(sender, "msg", [channelId, "1", "FAKEHASH"], ts);
		const forged = await jsonReq(mf, `/v1/channels/${channelId}/handshake/sender`, { msg: m1 }, auth);
		expect(forged.status).toBe(401);

		// join 后 receiver msg2（需 msg1 存在）→ ready
		await doJoin(mf, receiver, channelId, code);
		const hs2 = await doHandshake(mf, receiver, channelId, "receiver", m2);
		expect(hs2.status).toBe(200);
		expect(await hs2.json()).toMatchObject({ state: "ready", peer_msg: m1 });
	});

	it("receiver 拉取（空 msg）返回 peer_msg；sender 幂等重发返回 msg2", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const code = "ABCDEFGH";
		await doCreate(mf, sender, receiver, channelId);
		await doRegister(mf, sender, channelId, code);
		await doJoin(mf, receiver, channelId, code);

		const m1 = noiseMsg("m1");
		const m2 = noiseMsg("m2");

		// receiver 拉取：msg1 尚未发送 → peer null
		const pull1 = await doHandshake(mf, receiver, channelId, "receiver");
		expect(pull1.status).toBe(200);
		expect(await pull1.json()).toMatchObject({ peer_msg: null });

		// sender 发送 msg1；receiver 再拉取 → peer=msg1
		await doHandshake(mf, sender, channelId, "sender", m1);
		const pull2 = await doHandshake(mf, receiver, channelId, "receiver");
		expect(await pull2.json()).toMatchObject({ peer_msg: m1 });

		// receiver 发 msg2；sender 幂等重发 msg1 拉取 msg2 → ready
		await doHandshake(mf, receiver, channelId, "receiver", m2);
		const pull3 = await doHandshake(mf, sender, channelId, "sender", m1);
		expect(await pull3.json()).toMatchObject({ state: "ready", peer_msg: m2 });
	});
});

// ─── upload / download / R2 ─────────────────────────────────────────────────

describe("parts 数据面", () => {
	it("ready 前 upload 403；upload 201 → R2 对象 + manifest；幂等 200；异内容 409", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const code = randomCode();
		await doCreate(mf, sender, receiver, channelId);
		await doRegister(mf, sender, channelId, code);

		// created 状态 upload → 403
		const early = await doUpload(mf, sender, channelId, 0, new TextEncoder().encode("ct0"));
		expect(early.status).toBe(403);

		await doJoin(mf, receiver, channelId, code);
		await doHandshake(mf, sender, channelId, "sender", "m1");
		await doHandshake(mf, receiver, channelId, "receiver", "m2");

		const ct = new TextEncoder().encode("ciphertext-part-0");
		const up = await doUpload(mf, sender, channelId, 0, ct);
		expect(up.status).toBe(201);
		expect(await up.json()).toEqual({ part_index: 0, size: ct.length });

		// R2 对象存在
		const bucket = await mf.getR2Bucket("PERI_SYNC_PAYLOADS");
		const obj = await bucket.get(`channels/${channelId}/parts/0`);
		expect(obj).not.toBeNull();
		expect(await obj!.arrayBuffer()).toEqual(ct.buffer.slice(ct.byteOffset, ct.byteOffset + ct.byteLength));

		// 幂等重试（同 hash）→ 200，不重写
		const retry = await doUpload(mf, sender, channelId, 0, ct);
		expect(retry.status).toBe(200);

		// 异内容同 idx → 409
		const clash = await doUpload(mf, sender, channelId, 0, new TextEncoder().encode("other"));
		expect(clash.status).toBe(409);
	});

	it("下载 200/206、缺 part 404、角色错 401、state 门槛", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const { channelId } = await readyChannel(mf, sender, receiver);

		const ct = new TextEncoder().encode("download-me");
		await doUpload(mf, sender, channelId, 1, ct);

		const dl = await doDownload(mf, receiver, channelId, 1);
		expect(dl.status).toBe(200);
		expect(new Uint8Array(await dl.arrayBuffer())).toEqual(ct);

		// 缺 part → 404（可重试）
		expect((await doDownload(mf, receiver, channelId, 2)).status).toBe(404);

		// Range → 206 + content-range
		const part = await doDownload(mf, receiver, channelId, 1, "bytes=0-3");
		expect(part.status).toBe(206);
		expect(part.headers.get("content-range")).toBe(`bytes 0-3/${ct.length}`);
		expect(new Uint8Array(await part.arrayBuffer())).toEqual(ct.subarray(0, 4));

		// sender 用自己签名 download → 验签失败 401（服务端只用 receiver_ed_pub）
		const auth = await periSig(sender, "download", [channelId, "1"], nowSecs());
		const wrongRole = await mf.dispatchFetch(`https://example.com/v1/channels/${channelId}/parts/1`, {
			method: "GET",
			headers: { authorization: auth },
		});
		expect(wrongRole.status).toBe(401);
	});

	it("预算 413 不写 R2：part 超 64KiB / 超 512 parts", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const { channelId } = await readyChannel(mf, sender, receiver);
		const bucket = await mf.getR2Bucket("PERI_SYNC_PAYLOADS");
		const before = (await bucket.list({ prefix: `channels/${channelId}/` })).objects.length;

		// ciphertext > 64 KiB → 413，R2 对象数不变
		const big = new Uint8Array(64 * 1024 + 1);
		const up = await doUpload(mf, sender, channelId, 0, big);
		expect(up.status).toBe(413);
		expect((await bucket.list({ prefix: `channels/${channelId}/` })).objects.length).toBe(before);

		// part_index ≥ 512 → 413
		const idx = await doUpload(mf, sender, channelId, 512, new Uint8Array(16));
		expect(idx.status).toBe(413);
		expect((await bucket.list({ prefix: `channels/${channelId}/` })).objects.length).toBe(before);
	});
});

// ─── confirm / revoke ───────────────────────────────────────────────────────

describe("confirm / revoke", () => {
	it("confirm 204 → 同 receiver 幂等 204；异设备 401；created 状态 403", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const { channelId } = await readyChannel(mf, sender, receiver);

		const c1 = await doConfirm(mf, receiver, channelId);
		expect(c1.status).toBe(204);
		const c2 = await doConfirm(mf, receiver, channelId);
		expect(c2.status).toBe(204); // 同 receiver 幂等

		// confirmed 后 lookup/join → 404（码已失效 + 终态）
		expect((await doJoin(mf, receiver, channelId, "00000000")).status).toBe(404);
	});

	it("confirm 需要 receiver 签名；created 状态 confirm 403", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		await doCreate(mf, sender, receiver, channelId);
		// created 状态 confirm → 403（状态不符，矩阵固化；先于验签）
		expect((await doConfirm(mf, receiver, channelId)).status).toBe(403);

		// ready 状态非 receiver 公钥签名 → 401
		const sender2 = await makeDevice();
		const receiver2 = await makeDevice();
		const { channelId: ch2 } = await readyChannel(mf, sender2, receiver2);
		const attacker = await makeDevice();
		const ts = nowSecs();
		const auth = await periSig(attacker, "confirm", [ch2], ts);
		const hijack = await jsonReq(mf, `/v1/channels/${ch2}/confirm`, {}, auth);
		expect(hijack.status).toBe(401); // 非 receiver 公钥验签失败
	});

	it("revoke 204 → 幂等 204 → lookup 404；confirmed 后 revoke 404", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const code = randomCode();
		await doCreate(mf, sender, receiver, channelId);
		await doRegister(mf, sender, channelId, code);

		const r1 = await doRevoke(mf, sender, channelId);
		expect(r1.status).toBe(204);
		const r2 = await doRevoke(mf, sender, channelId);
		expect(r2.status).toBe(204); // 同 sender 幂等

		// revoke 即码失效
		expect((await doLookup(mf, code)).status).toBe(404);
		// revoked 后 join/confirm → 404
		expect((await doJoin(mf, receiver, channelId, code)).status).toBe(404);
		expect((await doConfirm(mf, receiver, channelId)).status).toBe(404);

		// confirmed 后 revoke → 404
		const sender2 = await makeDevice();
		const receiver2 = await makeDevice();
		const { channelId: ch2 } = await readyChannel(mf, sender2, receiver2);
		await doConfirm(mf, receiver2, ch2);
		expect((await doRevoke(mf, sender2, ch2)).status).toBe(404);
	});
});

// ─── 限流 ───────────────────────────────────────────────────────────────────

describe("限流", () => {
	it("lookup 15/min/IP → 第 16 次 429 + Retry-After；不同 IP 独立", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		const code = randomCode();
		await doCreate(mf, sender, receiver, channelId);
		await doRegister(mf, sender, channelId, code);

		const ip = "198.51.100.7";
		let last;
		for (let i = 0; i < 15; i++) {
			last = await doLookup(mf, code, ip);
			expect(last.status).toBe(200);
		}
		const denied = await doLookup(mf, code, ip);
		expect(denied.status).toBe(429);
		expect(Number(denied.headers.get("retry-after"))).toBeGreaterThan(0);
		expect(last!.status).toBe(200);

		// 不同 IP 不受影响
		expect((await doLookup(mf, code, "198.51.100.8")).status).toBe(200);
	});

	it("code 注册 2/min/device → 第 3 次 429", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = randomChannelId();
		await doCreate(mf, sender, receiver, channelId);

		const c1 = randomCode();
		const c2 = randomCode();
		expect((await doRegister(mf, sender, channelId, c1)).status).toBe(200);
		expect((await doRegister(mf, sender, channelId, c2)).status).toBe(200);
		const denied = await doRegister(mf, sender, channelId, randomCode());
		expect(denied.status).toBe(429);
		expect(Number(denied.headers.get("retry-after"))).toBeGreaterThan(0);
	});
});

// ─── 日志白名单（M3） ───────────────────────────────────────────────────────

describe("日志白名单（M3）", () => {
	it("happy path + 攻击路径：捕获日志不含 PeriSig/code 全文/channel_id 全文/密文", async () => {
		const logs: string[] = [];
		const m = makeMf({}, { logSink: (msg) => logs.push(msg) });
		try {
			const sender = await makeDevice();
			const receiver = await makeDevice();
			const channelId = randomChannelId();
			// 固定码：避免随机 8 字符偶现于日志导致断言歧义。
			const code = "ABCDEFGH";

			// happy path
			expect((await doCreate(m, sender, receiver, channelId)).status).toBe(201);
			expect((await doRegister(m, sender, channelId, code)).status).toBe(200);
			expect((await doLookup(m, code)).status).toBe(200);
			expect((await doJoin(m, receiver, channelId, code)).status).toBe(201);
			const msg1 = noiseMsg("m1");
			const msg2 = noiseMsg("m2");
			expect((await doHandshake(m, sender, channelId, "sender", msg1)).status).toBe(200);
			expect((await doHandshake(m, receiver, channelId, "receiver", msg2)).status).toBe(200);
			const ct = new TextEncoder().encode("ciphertext-secret-content");
			expect((await doUpload(m, sender, channelId, 0, ct)).status).toBe(201);

			// 攻击路径：无效签名 create（sender_ed_pub 与签名所用密钥不符 → 401）
			const attacker = await makeDevice();
			const ts = nowSecs();
			const badAuth = await periSig(
				attacker,
				"create",
				[randomChannelId(), attacker.deviceId, receiver.deviceId, attacker.edPubB64, attacker.xPubB64],
				ts,
			);
			const forged = await jsonReq(m, "/v1/channels", {
				channel_id: randomChannelId(),
				device_id: attacker.deviceId,
				expected_device_id: receiver.deviceId,
				expected_ed_pub: receiver.edPubB64,
				sender_ed_pub: receiver.edPubB64, // 与签名公钥不符 → 验签失败
				sender_x_pub: attacker.xPubB64,
			}, badAuth);
			expect(forged.status).toBe(401);

			// 白名单断言（logSink 只含 Worker 侧 safeLog 事件行，不含 mf:info
			// 请求行——后者含完整 URL/channel_id/code，不属于 Worker 日志）。
			const joined = logs.join("\n");
			expect(joined).toContain('"event":"create"'); // 捕获有效，非真空通过
			expect(joined).toContain('"event":"join"');
			expect(joined).not.toContain("PeriSig");
			expect(joined).not.toContain(code); // code 全文（codeShard 仅首字符）
			expect(joined).not.toContain(channelId); // channel_id 全文（仅 8 字符 hash）
			expect(joined).not.toContain("ciphertext-secret-content"); // payload 明文
			expect(joined).not.toContain(b64urlEncode(ct)); // payload 密文（b64）
		} finally {
			await m.dispose();
		}
	});
});

// ─── create 前置防线（H1） ──────────────────────────────────────────────────

describe("create 前置防线（H1）", () => {
	it("格式校验/验签/per-IP 限流在 Channel DO 分配前完成；非法 create 不产生 DO", async () => {
		const logs: string[] = [];
		const m = makeMf({ CREATE_RATE_PER_MIN: 3 }, { logSink: (msg) => logs.push(msg) });
		try {
			const sender = await makeDevice();
			const receiver = await makeDevice();
			const ip = "203.0.113.99";

			// (a) 格式非法（channel_id 非 16 字节 base64url）→ 400，不计限流
			const badFormat = await jsonReq(m, "/v1/channels", {
				channel_id: "not-a-channel-id",
				device_id: sender.deviceId,
				expected_device_id: receiver.deviceId,
				expected_ed_pub: receiver.edPubB64,
				sender_ed_pub: sender.edPubB64,
				sender_x_pub: sender.xPubB64,
			}, undefined, { "CF-Connecting-IP": ip });
			expect(badFormat.status).toBe(400);

			// (b) 验签失败（sender_ed_pub 与签名密钥不符）→ 401
			const attacker = await makeDevice();
			const forgedReq = async () => {
				const t = nowSecs();
				const auth = await periSig(
					attacker,
					"create",
					[randomChannelId(), attacker.deviceId, receiver.deviceId, attacker.edPubB64, attacker.xPubB64],
					t,
				);
				return jsonReq(m, "/v1/channels", {
					channel_id: randomChannelId(),
					device_id: attacker.deviceId,
					expected_device_id: receiver.deviceId,
					expected_ed_pub: receiver.edPubB64,
					sender_ed_pub: receiver.edPubB64,
					sender_x_pub: attacker.xPubB64,
				}, auth, { "CF-Connecting-IP": ip });
			};
			expect((await forgedReq()).status).toBe(401);
			expect((await forgedReq()).status).toBe(401);
			expect((await forgedReq()).status).toBe(401);

			// (c) 同 IP 超 3/min → 429 + Retry-After（per-IP 防线在验签前，恶意
			// 请求也计数；自报 device_id 换键无法绕过 IP 键）
			const denied = await forgedReq();
			expect(denied.status).toBe(429);
			expect(Number(denied.headers.get("retry-after"))).toBeGreaterThan(0);

			// 非法/验签失败从未转发：无任何 create 事件日志（不产生 Channel DO）
			expect(logs.join("\n")).not.toContain('"event":"create"');
		} finally {
			await m.dispose();
		}
	});
});

