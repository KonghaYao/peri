// alarm TTL 推进与清理测试（短 TTL bindings 加速，plan §1.6 env 配置门禁）。
// 覆盖：created 到期 → expired 终态；终态 tombstone 到期 → 彻底清理（全 404）；
// alarm 清理删除 R2 prefix（孤儿清理）；code 60s 有效期到期 → lookup 404。

import { afterAll, beforeAll, describe, expect, it } from "bun:test";
import type { Miniflare } from "miniflare";
import { b64urlEncode, normalizeSyncCode } from "./canonical";
import { sha256B64url } from "./crypto";
import {
	makeDevice,
	makeMf,
	nowSecs,
	periSig,
	sleep,
	type TestDevice,
} from "./test-utils";

const SHORT: Record<string, number> = {
	TTL_CREATED_SECS: 1,
	TTL_PAIRED_SECS: 1,
	TTL_READY_SECS: 1,
	TTL_TOMBSTONE_SECS: 1,
	CODE_VALID_SECS: 1,
};

let mf: Miniflare;

beforeAll(() => {
	mf = makeMf(SHORT);
});

afterAll(async () => {
	await mf.dispose();
});

// ─── 复用 channel-do.test.ts 的流程 helper（内联最小版） ───────────────────

function randomChannelId(): string {
	const bytes = new Uint8Array(16);
	crypto.getRandomValues(bytes);
	return b64urlEncode(bytes);
}

function randomCode(): string {
	const alpha = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
	let out = "";
	for (let i = 0; i < 8; i++) out += alpha[Math.floor(Math.random() * alpha.length)];
	return out;
}

async function create(m: Miniflare, sender: TestDevice, receiver: TestDevice) {
	const channelId = randomChannelId();
	const ts = nowSecs();
	const auth = await periSig(sender, "create", [channelId, sender.deviceId, receiver.deviceId, sender.edPubB64, sender.xPubB64], ts);
	const resp = await m.dispatchFetch("https://example.com/v1/channels", {
		method: "POST",
		headers: { "content-type": "application/json", authorization: auth },
		body: JSON.stringify({
			channel_id: channelId,
			device_id: sender.deviceId,
			expected_device_id: receiver.deviceId,
			expected_ed_pub: receiver.edPubB64,
			sender_ed_pub: sender.edPubB64,
			sender_x_pub: sender.xPubB64,
		}),
	});
	expect(resp.status).toBe(201);
	return channelId;
}

async function register(m: Miniflare, sender: TestDevice, channelId: string, code: string) {
	const t = nowSecs();
	const epoch = Math.floor(t / 30);
	const hash = await sha256B64url(new TextEncoder().encode(normalizeSyncCode(code)!));
	const auth = await periSig(sender, "code", [channelId, String(epoch), hash], t);
	return m.dispatchFetch(`https://example.com/v1/channels/${channelId}/code`, {
		method: "POST",
		headers: { "content-type": "application/json", authorization: auth },
		body: JSON.stringify({ code, epoch }),
	});
}

async function lookup(m: Miniflare, code: string) {
	return m.dispatchFetch(`https://example.com/v1/code/${encodeURIComponent(code)}/lookup`, {
		method: "POST",
		headers: { "CF-Connecting-IP": `203.0.113.${1 + Math.floor(Math.random() * 200)}` },
	});
}

async function join(m: Miniflare, receiver: TestDevice, channelId: string, code: string) {
	const t = nowSecs();
	const auth = await periSig(receiver, "join", [channelId, normalizeSyncCode(code)!, receiver.deviceId, receiver.edPubB64, receiver.xPubB64], t);
	return m.dispatchFetch(`https://example.com/v1/channels/${channelId}/join`, {
		method: "POST",
		headers: { "content-type": "application/json", authorization: auth },
		body: JSON.stringify({ code, device_id: receiver.deviceId, ed_pub: receiver.edPubB64, x_pub: receiver.xPubB64 }),
	});
}

async function handshake(m: Miniflare, dev: TestDevice, channelId: string, role: "sender" | "receiver", msg: string) {
	const seq = role === "sender" ? "1" : "2";
	const payload = new TextEncoder().encode(msg);
	const hash = await sha256B64url(payload);
	const t = nowSecs();
	const auth = await periSig(dev, "msg", [channelId, seq, hash], t);
	return m.dispatchFetch(`https://example.com/v1/channels/${channelId}/handshake/${role}`, {
		method: "POST",
		headers: { "content-type": "application/json", authorization: auth },
		body: JSON.stringify({ msg: b64urlEncode(payload) }),
	});
}

async function upload(m: Miniflare, sender: TestDevice, channelId: string, idx: number, ct: Uint8Array) {
	const hash = await sha256B64url(ct);
	const t = nowSecs();
	const auth = await periSig(sender, "upload", [channelId, String(idx), hash], t);
	return m.dispatchFetch(`https://example.com/v1/channels/${channelId}/parts`, {
		method: "POST",
		headers: { "content-type": "application/json", authorization: auth },
		body: JSON.stringify({ part_index: idx, ciphertext: b64urlEncode(ct) }),
	});
}

async function download(m: Miniflare, receiver: TestDevice, channelId: string, idx: number) {
	const t = nowSecs();
	const auth = await periSig(receiver, "download", [channelId, String(idx)], t);
	return m.dispatchFetch(`https://example.com/v1/channels/${channelId}/parts/${idx}`, {
		method: "GET",
		headers: { authorization: auth },
	});
}

describe("alarm TTL 推进", () => {
	it("created 到期 → expired 终态：join/confirm/revoke 404", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = await create(mf, sender, receiver);

		await sleep(1500); // TTL_CREATED_SECS=1 到期，alarm 置 expired

		expect((await join(mf, receiver, channelId, randomCode())).status).toBe(404);
		const t = nowSecs();
		const auth = await periSig(receiver, "confirm", [channelId], t); // I3：冻结字段序 [channel_id, ts]
		expect((await mf.dispatchFetch(`https://example.com/v1/channels/${channelId}/confirm`, { method: "POST", headers: { "content-type": "application/json", authorization: auth }, body: "{}" })).status).toBe(404);
		const a2 = await periSig(sender, "revoke", [channelId], nowSecs()); // I3：冻结字段序 [channel_id, ts]
		expect((await mf.dispatchFetch(`https://example.com/v1/channels/${channelId}/revoke`, { method: "POST", headers: { "content-type": "application/json", authorization: a2 }, body: "{}" })).status).toBe(404);
	});

	it("code 注册 60s 有效期：到期后 lookup 404", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = await create(mf, sender, receiver);
		const code = randomCode();

		expect((await register(mf, sender, channelId, code)).status).toBe(200);
		expect((await lookup(mf, code)).status).toBe(200);

		await sleep(1500); // CODE_VALID_SECS=1 到期

		expect((await lookup(mf, code)).status).toBe(404);
	});

	it("终态 tombstone 到期 → 彻底清理；alarm 清理删除 R2 prefix（孤儿清理）", async () => {
		const sender = await makeDevice();
		const receiver = await makeDevice();
		const channelId = await create(mf, sender, receiver);
		const code = randomCode();
		await register(mf, sender, channelId, code);
		expect((await join(mf, receiver, channelId, code)).status).toBe(201);
		await handshake(mf, sender, channelId, "sender", "noise-1");
		await handshake(mf, receiver, channelId, "receiver", "noise-2");

		const ct = new TextEncoder().encode("ciphertext");
		expect((await upload(mf, sender, channelId, 0, ct)).status).toBe(201);

		const bucket = await mf.getR2Bucket("PERI_SYNC_PAYLOADS");
		const prefix = `channels/${channelId}/`;
		expect((await bucket.list({ prefix })).objects.length).toBe(1);

		// ready/transferring TTL=1s 到期 → alarm 清理 R2 + 置 expired 终态
		await sleep(1500);
		expect((await bucket.list({ prefix })).objects.length).toBe(0); // R2 prefix 已清理
		expect((await download(mf, receiver, channelId, 0)).status).toBe(404);

		// expired tombstone=1s 到期 → deleteAll 彻底清理（仍 404）
		await sleep(1500);
		expect((await bucket.list({ prefix })).objects.length).toBe(0);
		expect((await download(mf, receiver, channelId, 0)).status).toBe(404);
	});
});
