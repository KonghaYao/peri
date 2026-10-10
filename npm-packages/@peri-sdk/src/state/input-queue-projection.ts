import { DocModel } from "./doc-model";
import { finite, object, string, type RecordValue } from "./protocol";

type QueueItem = { inputId: string; state: string };
type QueueView = { generation: string; revision: number; items: QueueItem[]; activeRequestId?: string };
const QUEUE_STATES = new Set(["queued", "dispatching", "claimed", "delivered", "withdrawn"]);

/** Queue observations have their own generation; user content never enters this projection. */
export class InputQueueProjection {
    constructor(private readonly docs: DocModel) {}

    seed(snapshot: unknown): boolean { return this.acceptSnapshot(snapshot); }

    acceptAgentEvent(type: string, value: RecordValue | null): boolean {
        if (type === "user_input_queue_changed") return this.acceptSnapshot(value?.snapshot);
        if (type !== "user_input_delivered" || !value) return false;
        const inputId = string(value.input_id);
        const generation = string(value.generation);
        const current = this.docs.info().get("inputQueue") as QueueView | undefined;
        if (!inputId || !generation || !current || generation !== current.generation) return false;
        if (!current.items.some((item) => item.inputId === inputId)) return false;
        this.docs.info().set("inputQueue", {
            ...current,
            items: current.items.map((item) => item.inputId === inputId ? { inputId, state: "delivered" } : item),
        } satisfies QueueView);
        return true;
    }

    private acceptSnapshot(raw: unknown): boolean {
        const snapshot = object(raw);
        const generation = string(snapshot?.generation);
        if (!generation || !Array.isArray(snapshot?.items)) return false;
        const revision = finite(snapshot.revision) ?? 0;
        const current = this.docs.info().get("inputQueue") as QueueView | undefined;
        if (current?.generation === generation && revision < current.revision) return false;
        const items: QueueItem[] = [];
        for (const rawItem of snapshot.items) {
            const item = object(rawItem);
            const inputId = string(item?.inputId);
            const state = string(item?.state);
            if (!inputId || !state || !QUEUE_STATES.has(state)) return false;
            items.push({ inputId, state });
        }
        const activeRequestId = string(snapshot.activeRequestId);
        this.docs.info().set("inputQueue", {
            generation, revision, items,
            ...(activeRequestId ? { activeRequestId } : {}),
        } satisfies QueueView);
        return true;
    }
}
