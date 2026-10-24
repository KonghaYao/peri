import type { HistoryPageItem, HistorySubItem } from "./history-merge";

/**
 * 运行侧历史浏览窗口（无 Yjs）。
 *
 * `snapshotToken` 是**同一次浏览**的范围身份：页与页、页与首屏只在同一个 token 内可比。
 * rewind／重新初始 load 之后旧 token 一律作废，旧范围响应整页丢弃。
 * `nextCursor` 由浏览器持有并推进，调用方只表达「还要更早的一页」。
 */

export type HistoryPageOutcome = { applied: number; nextCursor: string | null };

/** 翻页不可用的原因：能力未协商、没有更早内容，或浏览窗口已被 rewind／新 load 作废。 */
export type HistoryPageUnavailableReason = "not_negotiated" | "exhausted" | "invalidated";

/** 没有可用游标（未协商／已作废／确实没有更早内容）。 */
export class HistoryPageUnavailableError extends Error {
    constructor(readonly reason: HistoryPageUnavailableReason, message: string) {
        super(message);
        this.name = "HistoryPageUnavailableError";
    }
}

/** 迟到的历史页：会话已切换／已关闭，或浏览快照已变——整页丢弃，不得污染当前视图。 */
export class StaleHistoryPageError extends Error {
    constructor(readonly reason: "session" | "window", message: string) {
        super(message);
        this.name = "StaleHistoryPageError";
    }
}

/** 响应形状不符合 ACP 分页扩展契约。 */
export class HistoryPageMalformedError extends Error {
    constructor(message: string) {
        super(message);
        this.name = "HistoryPageMalformedError";
    }
}

/** 一页历史的内容与推进结果；`applied` 只按首次出现的 `entryId` 计数。 */
export type HistoryPageLoad = HistoryPageOutcome & { items: HistoryPageItem[] };

type HistoryWindow = { snapshotToken: string; nextCursor: string | null };

export type HistoryPageFetch = (sessionId: string, cursor: string, limit: number | undefined) => Promise<unknown>;

function record(value: unknown): Record<string, unknown> | null {
    return value !== null && typeof value === "object" && !Array.isArray(value)
        ? value as Record<string, unknown>
        : null;
}

function nonEmpty(value: unknown): string | null {
    return typeof value === "string" && value.length > 0 ? value : null;
}

/** 解析一页响应的条目；形状不符合契约时返回 `null`（调用方按坏响应处理）。 */
export function decodeHistoryPageItems(raw: unknown): HistoryPageItem[] | null {
    if (!Array.isArray(raw)) return null;
    const items: HistoryPageItem[] = [];
    for (const entry of raw) {
        const value = record(entry);
        const entryId = nonEmpty(value?.entryId);
        if (!value || !entryId || !Array.isArray(value.subItems)) return null;
        const position = value.position;
        const subItems: HistorySubItem[] = [];
        for (const rawSub of value.subItems) {
            const sub = record(rawSub);
            const update = record(sub?.update);
            if (!sub || !update) continue;
            const index = sub.position;
            subItems.push({
                position: typeof index === "number" && Number.isFinite(index) ? index : null,
                update,
            });
        }
        items.push({
            entryId,
            position: typeof position === "number" && Number.isFinite(position) ? position : null,
            ...(value.oversize === true ? { oversize: true } : {}),
            subItems,
        });
    }
    return items;
}

/**
 * 单次绑定的历史游标与去重窗口。所有状态都在运行侧；View 只接收 `history-page` 事实。
 */
export class HistoryBrowser {
    private window: HistoryWindow | null = null;
    /** 窗口被 rewind 作废：游标没了，但「范围失效」与「从未协商」是两回事。 */
    private invalidatedWindow = false;
    private readonly seenEntries = new Set<string>();
    /** 相同 cursor + 相同 limit 的并发翻页合并为一次请求。 */
    private readonly pendingPages = new Map<string, Promise<HistoryPageLoad>>();

    /** 用 `session/load` 响应初始化浏览窗口；响应未协商分页时窗口为空。 */
    seed(response: unknown): void {
        const value = record(response);
        const snapshotToken = nonEmpty(value?.snapshotToken);
        this.window = snapshotToken
            ? { snapshotToken, nextCursor: nonEmpty(value?.nextCursor) }
            : null;
        this.invalidatedWindow = false;
        this.seenEntries.clear();
    }

    get cursor(): string | null { return this.window?.nextCursor ?? null; }
    get hasWindow(): boolean { return this.window !== null; }
    get invalidated(): boolean { return this.invalidatedWindow; }

    /** rewind：范围真的变了，游标与浏览身份一起作废，翻页要等下一次初始 load 才恢复。 */
    invalidate(): void {
        this.window = null;
        this.invalidatedWindow = true;
        this.seenEntries.clear();
    }

    /** 关闭/换代清理；不改变「是否曾协商」的外部判断，由调用方丢弃实例。 */
    reset(): void {
        this.window = null;
        this.invalidatedWindow = false;
        this.seenEntries.clear();
        this.pendingPages.clear();
    }

    /**
     * 向上读取更早一页。响应必须属于当前会话与当前 `snapshotToken`；失败保留游标，
     * 可直接重试。并发调用只在 cursor + limit 相同时合并。
     */
    async load(
        sessionId: string,
        limit: number | undefined,
        fetch: HistoryPageFetch,
    ): Promise<HistoryPageLoad> {
        const window = this.window;
        const cursor = window?.nextCursor ?? null;
        if (!window || !cursor) {
            const reason: HistoryPageUnavailableReason = this.invalidatedWindow
                ? "invalidated"
                : window ? "exhausted" : "not_negotiated";
            throw new HistoryPageUnavailableError(reason, {
                invalidated: "History window is invalidated; reload the session before paging further",
                exhausted: "This browsing window has no earlier history left",
                not_negotiated: "This connection has no history pagination window",
            }[reason]);
        }
        const key = `${cursor}|${limit ?? "default"}`;
        const inFlight = this.pendingPages.get(key);
        if (inFlight) return inFlight;
        const pending = this.fetchPage(sessionId, cursor, window.snapshotToken, limit, fetch);
        this.pendingPages.set(key, pending);
        try {
            return await pending;
        } finally {
            if (this.pendingPages.get(key) === pending) this.pendingPages.delete(key);
        }
    }

    private async fetchPage(
        sessionId: string,
        cursor: string,
        snapshotToken: string,
        limit: number | undefined,
        fetch: HistoryPageFetch,
    ): Promise<HistoryPageLoad> {
        // 历史页请求只带 {sessionId, cursor, limit}：不重新装配、不改绑定。
        const response = await fetch(sessionId, cursor, limit);
        if (this.invalidatedWindow || !this.window || this.window.snapshotToken !== snapshotToken
            || this.window.nextCursor !== cursor)
            throw new StaleHistoryPageError("window", "History page belongs to an expired browsing snapshot");
        const value = record(response);
        const items = decodeHistoryPageItems(value?.items);
        if (!value || items === null)
            throw new HistoryPageMalformedError("session/load returned a malformed history page");
        const responseSession = nonEmpty(value.sessionId);
        if (responseSession !== null && responseSession !== sessionId)
            throw new StaleHistoryPageError("session", "History page belongs to another session");
        const responseToken = nonEmpty(value.snapshotToken);
        if (responseToken === null || responseToken !== snapshotToken)
            throw new StaleHistoryPageError("window", "History page belongs to an expired browsing snapshot");
        let applied = 0;
        for (const item of items) {
            if (this.seenEntries.has(item.entryId)) continue;
            this.seenEntries.add(item.entryId);
            applied++;
        }
        const nextCursor = nonEmpty(value.nextCursor);
        this.window = { snapshotToken, nextCursor };
        return { applied, nextCursor, items };
    }
}
