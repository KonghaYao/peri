export type RecordValue = Record<string, unknown>;
export type SnapshotBlock = { type: "text" | "reasoning"; text: string } | { type: "tool"; id: string; name: string; arguments?: unknown };
export type SnapshotMessage = {
    id: string;
    role: "user" | "assistant" | "tool";
    blocks: SnapshotBlock[];
    toolCallId?: string;
    isError?: boolean;
    resultText?: string;
};

export const object = (value: unknown): RecordValue | null =>
    value !== null && typeof value === "object" && !Array.isArray(value) ? value as RecordValue : null;
export const string = (value: unknown): string | null =>
    typeof value === "string" && value.length > 0 ? value : null;
export const finite = (value: unknown): number | null =>
    typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;

/** Keep only displayable text from standard ACP ToolCallContent wrappers. */
export function toolContentText(raw: unknown): string | null {
    if (!Array.isArray(raw)) return null;
    const parts: string[] = [];
    for (const item of raw) {
        const wrapper = object(item);
        if (!wrapper) continue;
        const block = wrapper.type === "content" ? object(wrapper.content) : wrapper;
        if (block?.type === "text" && typeof block.text === "string") parts.push(block.text);
    }
    return parts.length ? parts.join("\n") : null;
}

/** BaseMessage content is a string or a list of standard content blocks. */
function snapshotContentText(raw: unknown): string {
    if (typeof raw === "string") return raw;
    if (!Array.isArray(raw)) return "";
    return raw.flatMap((item) => {
        const block = object(item);
        return block?.type === "text" && typeof block.text === "string" ? [block.text] : [];
    }).join("");
}

/** Admit only public message fields; raw provider blocks and tool payloads stay out of Y.Doc. */
export function decodeMessagesJson(raw: unknown): SnapshotMessage[] | null {
    if (typeof raw !== "string") return null;
    let parsed: unknown;
    try { parsed = JSON.parse(raw); } catch { return null; }
    if (!Array.isArray(parsed)) return null;
    const messages: SnapshotMessage[] = [];
    for (const item of parsed) {
        const message = object(item);
        const id = string(message?.id);
        const role = message?.role;
        if (!message || !id || (role !== "user" && role !== "assistant" && role !== "system" && role !== "tool")) return null;
        if (role === "system") continue;
        const blocks: SnapshotBlock[] = [];
        if (typeof message.content === "string") {
            if (message.content) blocks.push({ type: "text", text: message.content });
        } else if (Array.isArray(message.content)) {
            for (const rawBlock of message.content) {
                const block = object(rawBlock);
                if (!block) continue;
                if ((block.type === "text" || block.type === "reasoning") && typeof block.text === "string") {
                    blocks.push({ type: block.type, text: block.text });
                } else if (block.type === "tool_use") {
                    const callId = string(block.id);
                    if (callId) blocks.push({ type: "tool", id: callId, name: string(block.name) ?? "Tool",
                        ...(block.input !== undefined ? { arguments: block.input } : {}) });
                }
            }
        } else return null;
        if (role === "assistant" && Array.isArray(message.tool_calls)) {
            for (const rawCall of message.tool_calls) {
                const call = object(rawCall);
                const callId = string(call?.id);
                if (callId && !blocks.some((block) => block.type === "tool" && block.id === callId)) {
                    blocks.push({ type: "tool", id: callId, name: string(call?.name) ?? "Tool",
                        ...(call?.arguments !== undefined ? { arguments: call.arguments } : {}) });
                }
            }
        }
        messages.push({ id, role, blocks,
            ...(role === "tool" ? { toolCallId: string(message.tool_call_id) ?? undefined,
                isError: message.is_error === true, resultText: snapshotContentText(message.content) } : {}),
        });
    }
    return messages;
}

export function decodeAgentEvent(raw: unknown): { type: string; value: RecordValue | null } | null {
    if (typeof raw !== "string") return null;
    let parsed: unknown;
    try { parsed = JSON.parse(raw); } catch { return null; }
    const event = object(parsed);
    const type = string(event?.type);
    return type ? { type, value: object(event?.value) } : null;
}
