// canonical.ts 单元测试：transcript 固定向量与 Crockford 归一化。
// 固定向量与 Rust `peri-tui/src/sync/canonical_test.rs` 一致。

import { describe, expect, it } from "bun:test";
import {
	PROTOCOL_TAG,
	b64urlDecode,
	b64urlEncode,
	normalizeSyncCode,
	timestampWithinSkew,
	transcript,
	utf8Bytes,
} from "./canonical";

describe("transcript", () => {
	it("复刻 Rust 固定向量（字段顺序、分隔符与时间戳逐字节稳定）", () => {
		expect(transcript("create", ["ch_abc", "dev_xyz"], 1715000000)).toBe(
			"peri-sync/v1|create|ch_abc|dev_xyz|1715000000",
		);
		expect(transcript("join", ["code_1", "ch_abc"], 0)).toBe(
			"peri-sync/v1|join|code_1|ch_abc|0",
		);
		expect(transcript("revoke", [], 42)).toBe("peri-sync/v1|revoke|42");
		expect(transcript("payload", ["c1", "3", "HASH"], 0)).toBe(
			"peri-sync/v1|payload|c1|3|HASH|0",
		);
	});

	it("PROTOCOL_TAG 固定", () => {
		expect(PROTOCOL_TAG).toBe("peri-sync/v1");
	});

	it("拒绝空 op / 空字段 / 含 | 片段", () => {
		expect(() => transcript("", ["a"], 1)).toThrow();
		expect(() => transcript("op|evil", ["a"], 1)).toThrow();
		expect(() => transcript("op", [""], 1)).toThrow();
		expect(() => transcript("op", ["a|b"], 1)).toThrow();
	});

	it("时间戳偏差判定（±300s 窗口）", () => {
		expect(timestampWithinSkew(1700, 2000, 300)).toBe(true);
		expect(timestampWithinSkew(2300, 2000, 300)).toBe(true);
		expect(timestampWithinSkew(1399, 2000, 300)).toBe(false);
		expect(timestampWithinSkew(2301, 2000, 300)).toBe(false);
	});
});

describe("b64url", () => {
	it("b64url-no-pad 固定向量（Rust 同）", () => {
		expect(b64urlEncode(utf8Bytes("hello"))).toBe("aGVsbG8");
		expect(b64urlEncode(utf8Bytes(""))).toBe("");
		expect(b64urlEncode(new Uint8Array([0xfb, 0xff, 0xfe]))).toBe("-__-");
		expect(b64urlEncode(new Uint8Array([0xfb, 0xff, 0xfe, 0x00]))).toBe("-__-AA");
	});

	it("解码往返一致；非法输入拒绝", () => {
		expect(b64urlEncode(b64urlDecode("aGVsbG8")!)).toBe("aGVsbG8");
		expect(b64urlDecode("aGVsbG8=")).toBeNull(); // 带 padding 拒绝
		expect(b64urlDecode("aGVs+bG8")).toBeNull(); // 标准 base64 字符拒绝
		expect(b64urlDecode("a")).toBeNull(); // 长度非法
	});
});

describe("Crockford 归一化", () => {
	it("示例码 7M4K-P9XQ 归一化为 8 字符", () => {
		expect(normalizeSyncCode("7M4K-P9XQ")).toBe("7M4KP9XQ");
		expect(normalizeSyncCode("7m4k-p9xq")).toBe("7M4KP9XQ");
	});

	it("O→0、I/L→1、大小写不敏感、去连字符；内部空格拒绝（与 Rust 一致）", () => {
		expect(normalizeSyncCode("OOOOOOOO")).toBe("00000000");
		expect(normalizeSyncCode("IIIIIIII")).toBe("11111111");
		expect(normalizeSyncCode("LLLLLLLL")).toBe("11111111");
		expect(normalizeSyncCode(" 7M4KP9XQ ")).toBe("7M4KP9XQ"); // 首尾空白 trim（Rust 同）
		expect(normalizeSyncCode("7M4K P9XQ")).toBeNull(); // 内部空格拒绝（Rust sync_code.rs 同）
	});

	it("拒绝 U、非法字符与长度≠8", () => {
		expect(normalizeSyncCode("7M4KU9XQ")).toBeNull(); // U
		expect(normalizeSyncCode("7M4K-P9X")).toBeNull(); // 长度 7
		expect(normalizeSyncCode("7M4K-P9XQA")).toBeNull(); // 长度 9
		expect(normalizeSyncCode("7M4K*P9XQ")).toBeNull();
		expect(normalizeSyncCode("")).toBeNull();
	});

	it("归一化结果均在字母表内", () => {
		for (const code of ["7M4KP9XQ", "01234567", "ABCDEFGH", "JKMNPQRV", "TWXYZ000"]) {
			const n = normalizeSyncCode(code);
			expect(n).toBe(code);
			expect(normalizeSyncCode(code.toLowerCase())).toBe(code);
		}
	});
});
