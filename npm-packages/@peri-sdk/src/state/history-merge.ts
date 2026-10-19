import * as Y from "yjs";
import { DocModel } from "./doc-model";
import { object, string, toolContentText, type RecordValue } from "./protocol";

/**
 * 历史投影（初始回放与 cursor 历史页共用）到 Chat 文档的幂等合并。
 *
 * 身份来自 ACP 分页扩展契约（`peri-acp/src/host/history_page.rs` 模块文档）：
 * - `entryId` = `{会话 id}:{payload id}`，一个持久 payload 只有一个身份 —— 回放里的
 *   `_meta.periEntryId` 与页条目的 `entryId` 是同一个字符串。去重按它做，**不能**只用
 *   messageId：一条 assistant 的多个 block 共享 messageId。
 * - `position` = 存储序键（不透明整数），只在同一个 snapshotToken 内可比，只用于排序与
 *   区间合并；继承区条目在本线程没有行，因此没有位置，永远排在有条目的自有条目之前。
 * - 子项位置是它在**本条 payload 自己的投影序列**中的下标（页给 `subItems[].position`；
 *   回放没有子项下标，用 `(messageId, 类型, 出现次序)` 派生出同一份稳定块身份
 *   `h:{messageId}:{type}:{slot}`，两条路径对同一子项收敛到同一个块）。
 *
 * 这里不推进任何实时状态：不建 active turn、不伪造用户输入、不碰 plan／任务／输入队列
 * （REQ-C01-6/7、REQ-C02、验收 A23／A24）。实时路径的入口仍然是 `ChatProjection`。
 */

/** 一条页条目 / 一次回放戳的条目身份。 */
export type HistoryEntryRef = {
    entryId: string;
    /** 存储序键；`null` = 继承区条目（本线程没有行）。 */
    position: number | null;
    oversize?: boolean;
};

/** 一个投影子项：`update` 与回放路径同源，`position` 是它在投影序列中的下标。 */
export type HistorySubItem = { position: number | null; update: RecordValue };

/** 历史投影下的工具状态：只有已加载结果的工具才是终态，其余是「结果未加载」。 */
export const AWAITING_RESULT_STATUS = "awaiting_result";

const TERMINAL_TOOL_STATUSES = new Set(["completed", "error", "cancelled"]);

/** 回放通知的 load 代次（`_meta.periLoadGeneration`，REQ-H04）。 */
export function historyGenerationOf(notification: { params?: unknown }): string | null {
    const params = object(notification.params);
    if (!params) return null;
    const updateMeta = object(object(params.update)?._meta);
    const paramsMeta = object(params._meta);
    return string(updateMeta?.periLoadGeneration) ?? string(paramsMeta?.periLoadGeneration);
}

/** 一条 `session/update` 的条目身份；没有 `periEntryId` 说明它不是历史投影。 */
export function historyEntryRefOf(update: RecordValue): HistoryEntryRef | null {
    const meta = object(update._meta);
    const entryId = string(meta?.periEntryId);
    if (!entryId) return null;
    const position = meta?.periPosition;
    return {
        entryId,
        position: typeof position === "number" && Number.isFinite(position) ? position : null,
        oversize: meta?.periOversize === true,
    };
}

/** 页响应里的一个条目（`{sessionId, snapshotToken, nextCursor, items}` 的 items 元素）。 */
export type HistoryPageItem = HistoryEntryRef & { subItems: HistorySubItem[] };

/** 解析一页响应的条目；形状不符合契约时返回 `null`（调用方按坏响应处理）。 */
export function decodeHistoryPageItems(raw: unknown): HistoryPageItem[] | null {
    if (!Array.isArray(raw)) return null;
    const items: HistoryPageItem[] = [];
    for (const entry of raw) {
        const value = object(entry);
        const entryId = string(value?.entryId);
        if (!value || !entryId) return null;
        const position = value.position;
        const subItems: HistorySubItem[] = [];
        if (!Array.isArray(value.subItems)) return null;
        for (const rawSub of value.subItems) {
            const sub = object(rawSub);
            const update = object(sub?.update);
            if (!sub || !update) continue;
            const index = sub.position;
            subItems.push({ position: typeof index === "number" && Number.isFinite(index) ? index : null, update });
        }
        items.push({
            entryId,
            position: typeof position === "number" && Number.isFinite(position) ? position : null,
            oversize: value.oversize === true,
            subItems,
        });
    }
    return items;
}

export class HistoryMerge {
    /** `historyEntryId` → 文档条目 id；未命中时扫描文档并补写（跨副本可重建）。 */
    private readonly entryIds = new Map<string, string>();

    constructor(private readonly docs: DocModel) {}

    reset(): void { this.entryIds.clear(); }

    /** 合并一条历史条目（幂等）：同 `entryId` 重复到达不产生副本，也不改变已有内容。 */
    applyEntry(ref: HistoryEntryRef, subItems: Iterable<HistorySubItem>): boolean {
        const applicable = [...subItems].filter((sub) => this.mergeable(sub.update));
        if (!applicable.length) return false;
        const entry = this.ensureEntry(ref, applicable);
        const turnId = string(entry.get("turnId"))!;
        let changed = false;
        for (const sub of applicable) changed = this.mergeSubItem(entry, turnId, sub) || changed;
        return changed;
    }

    private mergeable(update: RecordValue): boolean {
        const kind = string(update.sessionUpdate);
        if (kind === "user_message_chunk" || kind === "agent_message_chunk" || kind === "agent_thought_chunk") {
            const content = object(update.content);
            return typeof content?.text === "string" && content.text.length > 0;
        }
        if (kind === "tool_call" || kind === "tool_call_update") {
            const sourceAgentId = string(object(object(update._meta)?.peri)?.sourceAgentId);
            return !sourceAgentId && !!(string(update.toolCallId) ?? string(update.id));
        }
        return false;
    }

    private ensureEntry(ref: HistoryEntryRef, subItems: HistorySubItem[]): Y.Map<unknown> {
        const known = this.entryIds.get(ref.entryId);
        const existing = known ? this.docs.entries().get(known) : undefined;
        if (existing) return existing;
        for (const [id, entry] of this.docs.entries()) {
            if (string(entry.get("historyEntryId")) === ref.entryId) {
                this.entryIds.set(ref.entryId, id);
                return entry;
            }
        }
        const role = subItems.some((sub) => string(sub.update.sessionUpdate) === "user_message_chunk")
            ? "user" : "assistant";
        // 文档条目 id 直接用 wire 身份：同一个 payload 在回放与分页上落进同一个条目。
        const entry = this.docs.ensureEntry(ref.entryId, `history:${ref.entryId}`, role);
        entry.set("historyEntryId", ref.entryId);
        entry.set("status", "completed");
        // 条目级位置 = 存储序键（跨条目可比）；块级是 `historySubPosition`（投影下标，不跨条目比）。
        if (ref.position !== null) entry.set("historyPosition", ref.position);
        for (const sub of subItems) {
            const messageId = string(sub.update.messageId);
            if (messageId) { entry.set("messageId", messageId); break; }
        }
        this.orderEntry(ref.entryId, ref.position);
        if (ref.oversize) entry.set("historyOversize", true);
        this.entryIds.set(ref.entryId, ref.entryId);
        return entry;
    }

    /**
     * 按存储序把条目放进 `entryOrder`（`ensureEntry` 已先追加一份，这里负责就位）。
     *
     * `entryOrder` 维持三段：继承前缀（有条目身份、没有位置）→ 自有历史（按位置升序）→
     * 实时尾部（既无身份也无位置的条目）。插到第一个位置更大的自有条目之前；越过实时尾部
     * 起点同样停下——旧页永远不能排到实时输出之后。
     */
    private orderEntry(entryId: string, position: number | null): void {
        const order = this.docs.chatRoot().get("entryOrder") as Y.Array<string>;
        const appended = order.toArray().indexOf(entryId);
        if (appended >= 0) order.delete(appended, 1);
        if (position === null) { order.push([entryId]); return; }
        for (let index = 0; index < order.length; index++) {
            const other = this.docs.entries().get(order.get(index));
            const otherPosition = other?.get("historyPosition");
            if (typeof otherPosition === "number" && otherPosition > position) {
                order.insert(index, [entryId]);
                return;
            }
            if (string(other?.get("historyEntryId")) === null) {
                order.insert(index, [entryId]);
                return;
            }
        }
        order.push([entryId]);
    }

    private mergeSubItem(entry: Y.Map<unknown>, turnId: string, sub: HistorySubItem): boolean {
        const kind = string(sub.update.sessionUpdate);
        if (kind === "user_message_chunk" || kind === "agent_message_chunk" || kind === "agent_thought_chunk") {
            return this.mergeText(entry, sub, kind === "agent_thought_chunk" ? "reasoning" : "text");
        }
        return this.mergeTool(entry, turnId, sub);
    }

    /** 文本块：历史子项是完整正文（不是流式分片），因此写「设定」而非「追加」。 */
    private mergeText(entry: Y.Map<unknown>, sub: HistorySubItem, type: "text" | "reasoning"): boolean {
        const content = object(sub.update.content);
        const value = typeof content?.text === "string" ? content.text : "";
        if (!value) return false;
        const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
        const blockId = this.textBlockId(blocks, sub, type, value);
        const existing = blocks.get(blockId);
        if (existing) {
            const text = existing.get("text");
            if (text instanceof Y.Text) {
                if (text.toString() === value) return false;
                text.delete(0, text.length);
                text.insert(0, value);
            } else existing.set("text", value);
            return true;
        }
        const block = new Y.Map<unknown>();
        block.set("blockId", blockId);
        block.set("type", type);
        const text = new Y.Text();
        block.set("text", text);
        if (type === "reasoning") block.set("visibility", "summary");
        if (sub.position !== null) block.set("historySubPosition", sub.position);
        blocks.set(blockId, block);
        const order = entry.get("blockOrder") as Y.Array<string>;
        order.insert(this.blockIndex(entry, order, sub.position, blockId), [blockId]);
        text.insert(0, value);
        if (entry.get("status") !== "completed") entry.set("status", "completed");
        return true;
    }

    /**
     * 文本块的稳定身份：`h:{messageId}:{type}:{slot}`，两条路径对同一子项得到同一个块。
     *
     * `slot` 是该子项在**本条 payload 自己的投影序列**里、同一 `(messageId, 类型)` 中的
     * 出现次序：页路径按子项下标递增到达，回放路径按投影顺序逐条到达，因此先认领
     * 「另一条路径已经建过的同一个子项」，认领不到才占下一个空槽位。
     */
    private textBlockId(
        blocks: Y.Map<Y.Map<unknown>>,
        sub: HistorySubItem,
        type: "text" | "reasoning",
        value: string,
    ): string {
        const prefix = `h:${string(sub.update.messageId) ?? "unknown"}:${type}:`;
        const claimed = this.claimTextBlock(blocks, prefix, type, sub.position, value);
        if (claimed) return claimed;
        let slot = 0;
        for (const id of blocks.keys()) if (id.startsWith(prefix)) slot++;
        return `${prefix}${slot}`;
    }

    /**
     * 认领同一条子项在**另一条路径**上已经落库的块（跨路径身份统一，REQ-C01-3）。
     *
     * - 页路径（带子项下标）：先找同下标的块（重复页幂等）；没有则按正文认领回放先建的块
     *   （回放块没有下标标记），并补上本次下标，之后的页合并按下标命中。
     * - 回放路径（没有子项下标）：只按正文认领页先建的块（带下标标记）；回放自身的到达
     *   次序已经能给出稳定槽位，不按正文合并，避免把两条相同文本的不同子项并成一条。
     */
    private claimTextBlock(
        blocks: Y.Map<Y.Map<unknown>>,
        prefix: string,
        type: "text" | "reasoning",
        position: number | null,
        value: string,
    ): string | null {
        let unmapped: string | null = null;
        for (const [id, block] of blocks) {
            if (!id.startsWith(prefix) || block.get("type") !== type) continue;
            const text = block.get("text");
            if ((text instanceof Y.Text ? text.toString() : text) !== value) continue;
            const marked = block.get("historySubPosition");
            if (position === null) {
                if (typeof marked === "number") return id;
                continue;
            }
            if (marked === position) return id;
            if (marked === undefined && unmapped === null) unmapped = id;
        }
        if (unmapped === null) return null;
        blocks.get(unmapped)!.set("historySubPosition", position);
        return unmapped;
    }

    /** 工具块：按调用实例身份合并；终态不可回退（与实时路径同一规则）。 */
    private mergeTool(entry: Y.Map<unknown>, turnId: string, sub: HistorySubItem): boolean {
        const update = sub.update;
        const id = string(update.toolCallId) ?? string(update.id);
        if (!id) return false;
        const name = string(update.title) ?? string(update.name);
        const status = historyToolStatus(string(update.status));
        const tool = this.docs.ensureTool(id, turnId, name ?? "Tool");
        let changed = false;
        if (name && tool.get("name") !== name) { tool.set("name", name); changed = true; }
        const kind = string(update.kind);
        if (kind && tool.get("kind") !== kind) { tool.set("kind", kind); changed = true; }
        if (sub.position !== null && tool.get("historySubPosition") !== sub.position) {
            tool.set("historySubPosition", sub.position);
        }
        const previous = string(tool.get("status"));
        // 终态不可回退：已完成的工具不因更早的调用帧回到「正在运行」。
        if (previous !== status && !(previous !== null && TERMINAL_TOOL_STATUSES.has(previous))) {
            tool.set("status", status);
            changed = true;
        }
        if (update.rawInput !== undefined) { this.docs.payloads.set(tool, "arguments", update.rawInput); changed = true; }
        if (TERMINAL_TOOL_STATUSES.has(status)) {
            const contentText = toolContentText(update.content);
            const result: RecordValue = {};
            if (contentText !== null) result.contentText = status === "error" && !contentText.trim()
                ? "Tool execution failed" : contentText;
            else if (status === "error") result.contentText = "Tool execution failed";
            if (update.rawOutput !== undefined) result.rawOutput = update.rawOutput;
            if (Object.keys(result).length) { this.docs.payloads.set(tool, "result", result); changed = true; }
        }
        const blockId = `tool:${id}`;
        const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
        const existingBlock = blocks.get(blockId);
        if (existingBlock) {
            // 重复到达（重试页／结果先到、调用后到）：位置没变就不动顺序。
            if (sub.position !== null && existingBlock.get("historySubPosition") !== sub.position) {
                existingBlock.set("historySubPosition", sub.position);
                this.moveBlock(entry, blockId, sub.position);
            }
        } else {
            // 不复用 `addToolBlock`：它把块追加在末尾，而历史块要按投影下标就位。
            const block = new Y.Map<unknown>();
            block.set("blockId", blockId);
            block.set("type", "tool_call");
            block.set("toolCallId", id);
            if (sub.position !== null) block.set("historySubPosition", sub.position);
            blocks.set(blockId, block);
            const order = entry.get("blockOrder") as Y.Array<string>;
            order.insert(this.blockIndex(entry, order, sub.position, blockId), [blockId]);
            tool.set("entryId", entry.get("entryId"));
            changed = true;
        }
        return changed;
    }

    private moveBlock(entry: Y.Map<unknown>, blockId: string, position: number): void {
        const order = entry.get("blockOrder") as Y.Array<string>;
        const current = order.toArray().indexOf(blockId);
        if (current < 0) return;
        order.delete(current, 1);
        order.insert(this.blockIndex(entry, order, position, blockId), [blockId]);
    }

    /** 块在 `blockOrder` 里的插入下标：第一个位置更大的历史块之前；无位置者追加在末尾。 */
    private blockIndex(entry: Y.Map<unknown>, order: Y.Array<string>, position: number | null, blockId: string): number {
        if (position === null) return order.length;
        const blocks = entry.get("blocks") as Y.Map<Y.Map<unknown>>;
        for (let index = 0; index < order.length; index++) {
            const id = order.get(index);
            if (id === blockId) continue;
            const other = blocks.get(id)?.get("historySubPosition");
            if (typeof other === "number" && other > position) return index;
        }
        return order.length;
    }
}

/** wire 状态 → 历史投影状态：没有结果的工具不是「正在运行」，而是「结果未加载」。 */
function historyToolStatus(raw: string | null): string {
    if (raw === "completed") return "completed";
    if (raw === "failed" || raw === "error") return "error";
    if (raw === "cancelled") return "cancelled";
    return AWAITING_RESULT_STATUS;
}
