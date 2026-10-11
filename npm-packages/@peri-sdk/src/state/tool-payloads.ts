import type * as Y from "yjs";

/** An immutable JSON version, scoped to one SessionDocs lifetime. */
export type ToolPayloadRef = { id: string; bytes: number; preview: string };
export const TOOL_PAYLOAD_INLINE_BYTES = 4096;
const PREVIEW_CHARACTERS = 512;
type Field = "arguments" | "result";

/** Large values live outside the replicated hot document. Rust owns durable history. */
export class ToolPayloads {
    private readonly generation = crypto.randomUUID();
    private nextId = 0;
    private readonly values = new Map<string, string>();

    read(id: string): string | undefined { return this.values.get(id); }
    clear(): void { this.values.clear(); }

    set(tool: Y.Map<unknown>, field: Field, value: unknown): void {
        const json = JSON.stringify(value);
        if (json === undefined) throw new TypeError("Tool payload must be JSON serializable");
        const refKey = `${field}Ref`;
        const previous = tool.get(refKey) as ToolPayloadRef | undefined;
        if (previous && this.values.get(previous.id) === json) return;
        if (!previous && tool.has(field) && JSON.stringify(tool.get(field)) === json) return;
        const bytes = new TextEncoder().encode(json).byteLength;
        if (bytes <= TOOL_PAYLOAD_INLINE_BYTES) {
            // Take ownership: ACP callers must not mutate a value already inserted into Yjs.
            tool.set(field, JSON.parse(json));
            if (previous) tool.delete(refKey);
        } else {
            const id = `${this.generation}:${++this.nextId}`;
            const content = field === "result" && value && typeof value === "object"
                ? (value as Record<string, unknown>).contentText : undefined;
            const preview = (typeof content === "string" ? content : json).slice(0, PREVIEW_CHARACTERS);
            this.values.set(id, json);
            tool.set(refKey, { id, bytes, preview } satisfies ToolPayloadRef);
            if (tool.has(field)) tool.delete(field);
        }
        // Stale references return unavailable, never the content of a different version.
        if (previous) this.values.delete(previous.id);
    }
}
