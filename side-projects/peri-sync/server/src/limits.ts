/// <reference types="@cloudflare/workers-types" />
// 限额与 TTL 常量（r2-encrypted-transfer contract）。
//
// 数值镜像 Rust `peri-tui/src/sync/limits.rs`（Slice 0 冻结，此后仅经显式
// plan 修订）。部署默认值即下方冻结值；env 覆盖仅用于本地测试推进 alarm
// （03-plan.md §1.6 "env 配置门禁"），生产部署不得覆盖。
//
// 未冻结项（03-plan 未给数值）：非 lookup/code 的签名端点统一采用宽松
// 60/min/device 防滥用，部署门禁可收紧。

export interface RateLimits {
	/** code 注册限流（次/分钟/device）。冻结 2。 */
	codeRegisterMaxPerMin: number;
	/** code lookup 限流（次/分钟/IP）。冻结 15。 */
	codeLookupRateLimitPerMin: number;
	/** 其他签名端点宽松限流（次/分钟/device）。未冻结，部署门禁可调。 */
	signedEndpointRateLimitPerMin: number;
	/** create 端点限流（次/分钟/device）。未冻结，部署门禁可调。 */
	createRateLimitPerMin: number;
}

export interface TtlLimits {
	/** created 状态 TTL（秒）。冻结 600。 */
	createdSecs: number;
	/** paired（join 成功/握手进行中）TTL（秒）。冻结 300。 */
	pairedSecs: number;
	/** ready/transferring TTL（秒）。冻结 3600。 */
	readySecs: number;
	/** 终态 tombstone（秒）。冻结 3600。 */
	tombstoneSecs: number;
}

export interface Limits extends RateLimits, TtlLimits {
	/** 签名时间戳可接受偏差（±300 秒，服务端侧）。 */
	signatureSkewSecs: number;
	/** 单个 payload part（AEAD envelope）上限（64 KiB）。 */
	maxPartBytes: number;
	/** 每 channel 最大 part 数（512）。 */
	maxPartsPerChannel: number;
	/**
	 * 每 channel payload 总预算：64 KiB × 512 = 32 MiB（实测对齐，
	 * L2 修订）；Slice 3 Rust 客户端按 32 MiB 分片，两侧一致。
	 */
	maxPayloadBytes: number;
	/** 单条 handshake opaque 消息上限（4 KiB，Noise blob 富余）。 */
	maxMsgBytes: number;
	/** 入口统一 body 大小上限（128 KiB，覆盖 b64 编码后的 part）。 */
	maxBodyBytes: number;
	/** code 自注册时刻起有效（秒）。冻结 60。 */
	codeValidSecs: number;
}

function readInt(env: Record<string, unknown> | undefined, key: string, fallback: number): number {
	if (!env) return fallback;
	const raw = env[key];
	if (typeof raw === "string" && /^\d+$/.test(raw)) return parseInt(raw, 10);
	if (typeof raw === "number" && Number.isFinite(raw) && raw >= 0) return raw;
	return fallback;
}

export function limits(env?: Record<string, unknown>): Limits {
	return {
		createdSecs: readInt(env, "TTL_CREATED_SECS", 600),
		pairedSecs: readInt(env, "TTL_PAIRED_SECS", 300),
		readySecs: readInt(env, "TTL_READY_SECS", 3600),
		tombstoneSecs: readInt(env, "TTL_TOMBSTONE_SECS", 3600),
		codeValidSecs: readInt(env, "CODE_VALID_SECS", 60),
		signatureSkewSecs: readInt(env, "SIGNATURE_SKEW_SECS", 300),
		maxPartBytes: readInt(env, "MAX_PART_BYTES", 64 * 1024),
		maxPartsPerChannel: readInt(env, "MAX_PARTS_PER_CHANNEL", 512),
		maxPayloadBytes: readInt(env, "MAX_PAYLOAD_BYTES", 32 * 1024 * 1024),
		maxMsgBytes: readInt(env, "MAX_MSG_BYTES", 4 * 1024),
		maxBodyBytes: readInt(env, "MAX_BODY_BYTES", 128 * 1024),
		codeRegisterMaxPerMin: readInt(env, "CODE_REGISTER_MAX_PER_MIN", 2),
		codeLookupRateLimitPerMin: readInt(env, "CODE_LOOKUP_RATE_LIMIT_PER_MIN", 15),
		signedEndpointRateLimitPerMin: readInt(env, "SIGNED_ENDPOINT_RATE_PER_MIN", 60),
		createRateLimitPerMin: readInt(env, "CREATE_RATE_PER_MIN", 60),
	};
}
