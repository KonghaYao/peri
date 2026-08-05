/// <reference types="@cloudflare/workers-types" />
// Ed25519 签名验证（WebCrypto，零 npm 密码依赖）。
//
// 签名头：`Authorization: PeriSig <device_id> <unix_ts> <signature>`
// 签名 bytes = `peri-sync/v1|op|field...|unix_seconds`（canonical.ts）。
// 服务端只验签，绝不接触私钥/同步码明文/data key/明文 payload。
//
// 验签失败统一返回 401（签名无效）；"签名有效但角色/状态/码窗/设备不符"
// 由调用方在验签通过后以 403 处理（H1 固化：请求自报公钥绝不用于验签）。

import { b64urlDecode, b64urlEncode, transcript, timestampWithinSkew, utf8Bytes } from "./canonical";

export const SIGNATURE_SCHEME = "PeriSig";

export type AuthOutcome = { ok: true; deviceId: string; ts: number } | { ok: false };

/** 拷贝为独立 ArrayBuffer（满足 WebCrypto BufferSource 的 ArrayBuffer 约束）。 */
function ab(bytes: Uint8Array): ArrayBuffer {
	const copy = new Uint8Array(bytes.byteLength);
	copy.set(bytes);
	return copy.buffer;
}

/** 解析 `PeriSig <device_id> <unix_ts> <sig>`；头缺失/格式错/ts 超窗 → 失败。 */
export function parsePeriSig(header: string | null, nowSecs: number, skewSecs: number): AuthOutcome {
	if (!header) return { ok: false };
	const parts = header.trim().split(/\s+/);
	if (parts.length !== 4 || parts[0] !== SIGNATURE_SCHEME) return { ok: false };
	const [, deviceId, tsStr, sig] = parts;
	if (!/^\d+$/.test(tsStr)) return { ok: false };
	const ts = parseInt(tsStr, 10);
	if (!timestampWithinSkew(ts, nowSecs, skewSecs)) return { ok: false };
	const deviceBytes = b64urlDecode(deviceId);
	if (deviceBytes === null || deviceBytes.length !== 16) return { ok: false };
	if (b64urlDecode(sig) === null) return { ok: false };
	return { ok: true, deviceId, ts };
}

/**
 * 以给定公钥验证 PeriSig 签名。`edPubB64` 必须来自服务端记录（create 登记
 * 的 sender_ed_pub / expected_ed_pub / receiver_ed_pub），绝不用请求自报公钥。
 * 任何篡改/密钥不符/字段非法均失败。
 */
export async function verifyPeriSig(
	edPubB64: string,
	header: string | null,
	op: string,
	fields: string[],
	nowSecs: number,
	skewSecs: number,
): Promise<AuthOutcome> {
	const parsed = parsePeriSig(header, nowSecs, skewSecs);
	if (!parsed.ok) return { ok: false };
	const msg = transcript(op, fields, parsed.ts);
	try {
		const pubBytes = b64urlDecode(edPubB64);
		if (pubBytes === null || pubBytes.length !== 32) return { ok: false };
		const sigBytes = b64urlDecode(header!.trim().split(/\s+/)[3]!);
		if (sigBytes === null || sigBytes.length !== 64) return { ok: false };
		const key = await crypto.subtle.importKey(
			"raw",
			ab(pubBytes),
			{ name: "Ed25519" },
			false,
			["verify"],
		);
		const valid = await crypto.subtle.verify(
			{ name: "Ed25519" },
			key,
			ab(sigBytes),
			ab(utf8Bytes(msg)),
		);
		return valid ? { ok: true, deviceId: parsed.deviceId, ts: parsed.ts } : { ok: false };
	} catch {
		return { ok: false };
	}
}

/** SHA-256，base64url-no-pad（服务端实算核对，不信任客户端声明）。 */
export async function sha256B64url(bytes: Uint8Array): Promise<string> {
	const digest = await crypto.subtle.digest("SHA-256", ab(bytes));
	return b64urlEncode(new Uint8Array(digest));
}
