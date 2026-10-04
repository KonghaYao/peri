import { createHash } from "node:crypto";

export const workload = Object.freeze({
    version: 1,
    tools: 12_000,
    toolsPerAssistant: 10,
    inputDataBytes: 960,
    resultTextBytes: 4_096,
    resultDataBytes: 4_096,
    asyncUpdates: 128,
    streamChunks: 16_384,
    chunkBytes: 1_024,
    largeResultBytes: 1_048_576,
});

/** ASCII with per-record identity: deterministic byte sizes without random or external data. */
export function content(index: number, bytes: number, field: string): string {
    const unit = `${field}:${index.toString(36).padStart(8, "0")}:abcdefghijklmnopqrstuvwxyz0123456789|`;
    return unit.repeat(Math.ceil(bytes / unit.length)).slice(0, bytes);
}

export function update(kind: string, fields: Record<string, unknown>) {
    return { jsonrpc: "2.0", method: "session/update", params: {
        sessionId: "benchmark-session", update: { sessionUpdate: kind, ...fields },
    } };
}

export function user() {
    return update("user_message_chunk", {
        messageId: "benchmark-user", content: { type: "text", text: "benchmark task" },
    });
}

export function assistant(index: number, text = "working\n") {
    return update("agent_message_chunk", {
        messageId: `assistant-${index}`, content: { type: "text", text },
    });
}

export function tool(index: number, large = false) {
    const id = `tool-${index}`;
    const argumentsValue = { index, path: `file-${index}.txt`, data: content(index, workload.inputDataBytes, "input") };
    const contentText = content(index, large ? workload.largeResultBytes : workload.resultTextBytes, "result");
    const rawOutput = large ? { index, bytes: contentText.length }
        : { index, data: content(index, workload.resultDataBytes, "output") };
    return {
        id,
        argumentsValue,
        resultValue: { contentText, rawOutput },
        start: update("tool_call", {
            toolCallId: id, title: "Read", kind: "read", status: "in_progress", rawInput: argumentsValue,
        }),
        finish: update("tool_call_update", {
            toolCallId: id, status: "completed", rawOutput,
            content: [{ type: "content", content: { type: "text", text: contentText } }],
        }),
    };
}

export function digest(text: string): string { return createHash("sha256").update(text).digest("hex"); }
export function byteLength(value: unknown): number { return Buffer.byteLength(JSON.stringify(value)); }

export function summarize(values: number[]) {
    if (!values.length) return { count: 0, total: 0, min: null, median: null, p95: null, max: null };
    const sorted = [...values].sort((a, b) => a - b);
    return {
        count: values.length, total: values.reduce((sum, value) => sum + value, 0),
        min: sorted[0], median: sorted[Math.floor(sorted.length / 2)],
        p95: sorted[Math.ceil(sorted.length * 0.95) - 1], max: sorted.at(-1),
    };
}
