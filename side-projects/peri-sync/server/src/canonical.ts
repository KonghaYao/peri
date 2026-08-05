/// <reference types="@cloudflare/workers-types" />
// canonical transcript 与 Crockford 归一化（r2-encrypted-transfer v1）。
//
// 签名 bytes 采用固定 UTF-8 canonical transcript：
// `peri-sync/v1|op|field...|unix_seconds`，'|' 分隔，二进制字段一律
// base64url-no-pad。本模块逐字节复刻 Rust `peri-tui/src/sync/canonical.rs`
// 的格式语义；各 op 的字段顺序即契约（冻结于 plan §1.1 + 03-plan 修订），
// Slice 3 Rust 客户端必须逐字节一致。

export const PROTOCOL_TAG = "peri-sync/v1";

/** 二进制字段编码：base64url，无 padding。 */
export function b64urlEncode(bytes: Uint8Array): string {
	let s = "";
	for (let i = 0; i < bytes.length; i += 0x8000) {
		s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
	}
	return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** base64url-no-pad 解码；任何非法字符/填充即返回 null。 */
export function b64urlDecode(s: string): Uint8Array | null {
	if (s.length === 0) return new Uint8Array(0);
	if (!/^[A-Za-z0-9_-]+$/.test(s)) return null;
	let b64 = s.replace(/-/g, "+").replace(/_/g, "/");
	while (b64.length % 4 !== 0) b64 += "=";
	try {
		const bin = atob(b64);
		const out = new Uint8Array(bin.length);
		for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
		return out;
	} catch {
		return null;
	}
}

export function utf8Bytes(s: string): Uint8Array {
	return new TextEncoder().encode(s);
}

/**
 * 组装带时间戳的签名 transcript：`peri-sync/v1|op|field...|unix_seconds`。
 * 字段必须非空且不含 '|'；op 非空且不含 '|'。时间戳为十进制 unix 秒。
 */
export function transcript(op: string, fields: string[], unixSecs: number): string {
	if (op.length === 0 || op.includes("|")) {
		throw new Error("invalid op");
	}
	for (const f of fields) {
		if (f.length === 0 || f.includes("|")) {
			throw new Error("invalid transcript field");
		}
	}
	const parts = [PROTOCOL_TAG, op, ...fields, String(unixSecs)];
	return parts.join("|");
}

/** 签名时间戳是否在 ±skew 内（服务端接受 ±300 秒）。 */
export function timestampWithinSkew(ts: number, nowSecs: number, skewSecs: number): boolean {
	return Math.abs(nowSecs - ts) <= skewSecs;
}

// ─── Crockford Base32（8 字符 locator）─────────────────────────────────────

/** Crockford 字母表（O→0、I/L→1；无 U）。 */
export const CROCKFORD_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/**
 * 归一化同步码：trim 首尾空白、去连字符、大写、O→0、I/L→1；归一化后必须
 * 恰好 8 个字母表字符，否则返回 null。与 Rust `sync_code.rs::normalize`
 * 一致：只忽略连字符，内部空格/`U`/其他字符一律拒绝（不再整体移除空格）。
 */
export function normalizeSyncCode(input: string): string | null {
	const cleaned = input.trim().replace(/-/g, "").toUpperCase();
	if (cleaned.length !== 8) return null;
	let out = "";
	for (const ch of cleaned) {
		if (ch >= "0" && ch <= "9") {
			out += ch;
		} else if (ch === "O") {
			out += "0";
		} else if (ch === "I" || ch === "L") {
			out += "1";
		} else if (ch >= "A" && ch <= "Z" && !"ILOU".includes(ch)) {
			out += ch;
		} else {
			return null;
		}
	}
	// 上述分支已保证字符 ∈ 字母表；防御性再确认一次。
	for (const ch of out) {
		if (!CROCKFORD_ALPHABET.includes(ch)) return null;
	}
	return out;
}

/** channel_id：16 字节 CSPRNG 的 base64url-no-pad 表示（22 字符）。 */
export function isValidChannelId(id: string): boolean {
	const bytes = b64urlDecode(id);
	return bytes !== null && bytes.length === 16;
}

/** device_id：16 字节 base64url-no-pad（22 字符）。 */
export function isValidDeviceId(id: string): boolean {
	return isValidChannelId(id);
}

/** Ed25519 公钥：32 字节 base64url-no-pad（43 字符）。 */
export function isValidPubKey(pub: string): boolean {
	const bytes = b64urlDecode(pub);
	return bytes !== null && bytes.length === 32;
}
