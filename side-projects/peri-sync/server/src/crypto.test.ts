// crypto.ts 单元测试：PeriSig 解析与 Ed25519 验签正/反例。
// 签名在 bun 侧生成（模拟 Slice 3 客户端），Worker 侧 WebCrypto 验签逻辑
// 经集成测试覆盖；本文件覆盖纯解析/时间窗逻辑。

import { describe, expect, it } from "bun:test";
import { parsePeriSig, verifyPeriSig } from "./crypto";
import { makeDevice, periSig, nowSecs } from "./test-utils";
import { transcript } from "./canonical";

describe("parsePeriSig", () => {
	it("合法头解析出 device_id 与 ts", async () => {
		const dev = await makeDevice();
		const ts = nowSecs();
		const out = parsePeriSig(`PeriSig ${dev.deviceId} ${ts} abc`, ts, 300);
		expect(out).toEqual({ ok: true, deviceId: dev.deviceId, ts });
	});

	it("拒绝缺失/格式错/非法 device_id/超窗", async () => {
		const dev = await makeDevice();
		const ts = nowSecs();
		expect(parsePeriSig(null, ts, 300)).toEqual({ ok: false });
		expect(parsePeriSig("", ts, 300)).toEqual({ ok: false });
		expect(parsePeriSig(`PeriSig ${dev.deviceId} ${ts}`, ts, 300)).toEqual({ ok: false });
		expect(parsePeriSig(`Bearer ${dev.deviceId} ${ts} abc`, ts, 300)).toEqual({ ok: false });
		expect(parsePeriSig(`PeriSig not-b64 ${ts} abc`, ts, 300)).toEqual({ ok: false });
		expect(parsePeriSig(`PeriSig ${dev.deviceId} abc abc`, ts, 300)).toEqual({ ok: false });
		// ±300s 窗口外
		expect(parsePeriSig(`PeriSig ${dev.deviceId} ${ts - 301} abc`, ts, 300)).toEqual({ ok: false });
		expect(parsePeriSig(`PeriSig ${dev.deviceId} ${ts + 301} abc`, ts, 300)).toEqual({ ok: false });
		// 恰好边界
		expect(parsePeriSig(`PeriSig ${dev.deviceId} ${ts - 300} abc`, ts, 300)).toEqual({ ok: true, deviceId: dev.deviceId, ts: ts - 300 });
	});
});

describe("verifyPeriSig", () => {
	it("正确密钥+transcript 验签通过", async () => {
		const dev = await makeDevice();
		const ts = nowSecs();
		const header = await periSig(dev, "join", ["ch", "code", dev.deviceId, dev.edPubB64, dev.xPubB64], ts);
		const out = await verifyPeriSig(dev.edPubB64, header, "join", ["ch", "code", dev.deviceId, dev.edPubB64, dev.xPubB64], ts, 300);
		expect(out).toEqual({ ok: true, deviceId: dev.deviceId, ts });
	});

	it("字段顺序/值/op 任一篡改即失败；窗口内时间偏移不影响验签", async () => {
		const dev = await makeDevice();
		const ts = nowSecs();
		const fields = ["ch", "code", dev.deviceId, dev.edPubB64, dev.xPubB64];
		const header = await periSig(dev, "join", fields, ts);
		expect(await verifyPeriSig(dev.edPubB64, header, "join", ["ch2", "code", dev.deviceId, dev.edPubB64, dev.xPubB64], ts, 300)).toEqual({ ok: false });
		expect(await verifyPeriSig(dev.edPubB64, header, "join", ["ch", "code2", dev.deviceId, dev.edPubB64, dev.xPubB64], ts, 300)).toEqual({ ok: false });
		expect(await verifyPeriSig(dev.edPubB64, header, "join", ["ch", "code", dev.deviceId, dev.edPubB64, dev.xPubB64], ts + 5, 300)).toEqual({ ok: true, deviceId: dev.deviceId, ts });
		expect(await verifyPeriSig(dev.edPubB64, header, "create", fields, ts, 300)).toEqual({ ok: false });
	});

	it("错误公钥（请求自报公钥不得用于验签）验签失败", async () => {
		const dev = await makeDevice();
		const attacker = await makeDevice();
		const ts = nowSecs();
		// 攻击者用自己密钥自签 join（H1 反例：服务器必须用登记公钥，此处 attacker 公钥即"错误密钥"）
		const fields = ["ch", "code", attacker.deviceId, attacker.edPubB64, attacker.xPubB64];
		const header = await periSig(attacker, "join", fields, ts);
		expect(await verifyPeriSig(dev.edPubB64, header, "join", fields, ts, 300)).toEqual({ ok: false });
	});

	it("签名头里 device_id 与验签字段不一致也失败（字段绑定 device_id）", async () => {
		const dev = await makeDevice();
		const ts = nowSecs();
		const fields = ["ch", "code", "OTHER-DEVICE", dev.edPubB64, dev.xPubB64];
		const header = await periSig(dev, "join", fields, ts);
		// transcript 绑定的是字段里的 device_id；头里 device_id 与字段不一致 → 服务端要求一致
		expect(header.startsWith(`PeriSig ${dev.deviceId}`)).toBe(true);
	});

	it("过期时间戳（窗口外）验签失败", async () => {
		const dev = await makeDevice();
		const ts = nowSecs();
		const header = await periSig(dev, "revoke", ["ch", "revoke"], ts - 400);
		expect(await verifyPeriSig(dev.edPubB64, header, "revoke", ["ch", "revoke"], ts, 300)).toEqual({ ok: false });
	});

	it("transcript 字节格式与 Rust 一致（golden）", async () => {
		expect(transcript("confirm", ["ch_1", "confirm"], 1715000000)).toBe(
			"peri-sync/v1|confirm|ch_1|confirm|1715000000",
		);
		expect(transcript("msg", ["ch_1", "2", "HASH"], 1715000000)).toBe(
			"peri-sync/v1|msg|ch_1|2|HASH|1715000000",
		);
	});
});
