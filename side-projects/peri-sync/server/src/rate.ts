/// <reference types="@cloudflare/workers-types" />
// SQLite rate 表限流（DO 内硬上限）。
//
// 03-plan：匿名 lookup 按 IP；签名端点按 device_id:route；429 必带 Worker
// 自算 Retry-After；DO 内清理过期 rate 行。窗口固定 60 秒。

export interface RateRow extends Record<string, SqlStorageValue> {
	key: string;
	window_start: number;
	count: number;
}

/**
 * 滑动（固定）窗口限流：`rate(key, route, limitPerMin)`。
 * 返回 `{ allowed: false, retryAfter }` 时调用方必须以 429 + Retry-After 响应。
 * 过期行惰性清理：窗口过期重开，且顺带删除超过 1 小时的旧行。
 */
export function rateLimit(
	sql: SqlStorage,
	key: string,
	route: string,
	limitPerMin: number,
	nowSecs: number,
): { allowed: true } | { allowed: false; retryAfter: number } {
	const table = `rate_${route.replace(/[^A-Za-z0-9_]/g, "_")}`;
	// 惰性清理：删除超过 1 小时的过期行（含其他 key）。
	sql.exec(`DELETE FROM ${table} WHERE window_start < ?`, nowSecs - 3600);
	const rows = sql
		.exec<RateRow>(`SELECT key, window_start, count FROM ${table} WHERE key = ?`, key)
		.toArray();
	const row = rows[0] ?? null;
	if (!row) {
		sql.exec(
			`INSERT INTO ${table} (key, window_start, count) VALUES (?, ?, 1)`,
			key,
			nowSecs,
		);
		return { allowed: true };
	}
	if (nowSecs - row.window_start >= 60) {
		sql.exec(`UPDATE ${table} SET window_start = ?, count = 1 WHERE key = ?`, nowSecs, key);
		return { allowed: true };
	}
	if (row.count >= limitPerMin) {
		const retryAfter = Math.max(1, 60 - (nowSecs - row.window_start));
		return { allowed: false, retryAfter };
	}
	sql.exec(`UPDATE ${table} SET count = count + 1 WHERE key = ?`, key);
	return { allowed: true };
}

/** 建 rate 表（每个 route 一张，避免 key 冲突语义混乱）。 */
export function createRateTables(sql: SqlStorage, routes: string[]): void {
	for (const route of routes) {
		const table = `rate_${route.replace(/[^A-Za-z0-9_]/g, "_")}`;
		sql.exec(
			`CREATE TABLE IF NOT EXISTS ${table} (
				key TEXT PRIMARY KEY,
				window_start INTEGER NOT NULL,
				count INTEGER NOT NULL
			)`,
		);
	}
}
