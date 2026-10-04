import { invariant, type Budget } from "./harness";
import { tool, workload } from "./workloads";

/** Public consumer path: resolve references carried by a cold replica using the owner API. */
function fieldValue(card: any, field: "arguments" | "result", owner: any, reads: any): unknown {
    if (card[field] !== undefined) return card[field];
    const ref = card[`${field}Ref`];
    invariant(ref && typeof ref.id === "string" && Number.isSafeInteger(ref.bytes), `${card.toolCallId} ${field} reference`);
    invariant(typeof owner.readPayload === "function", "reference owner exposes readPayload");
    const json = owner.readPayload(ref.id);
    invariant(typeof json === "string", `${ref.id} exact payload available`);
    invariant(Buffer.byteLength(json) === ref.bytes, `${ref.id} byte length`);
    reads.onDemandReads++;
    reads.onDemandReadBytes += ref.bytes;
    reads.referencedPayloadBytes += ref.bytes;
    return JSON.parse(json);
}

export function firstTool(snapshot: any): any | null {
    for (const entry of snapshot.entries) for (const block of entry.blocks) {
        if (block.type === "tool_call") return block.tool;
    }
    return null;
}

export function validateHistory(snapshot: any, owner: any, count: number, large: boolean, budget: Budget) {
    const reads = { onDemandReads: 0, onDemandReadBytes: 0, referencedPayloadBytes: 0 };
    const userEntries = snapshot.entries.filter((entry: any) => entry.role === "user");
    invariant(userEntries.length === 1, "one historical user entry");
    invariant(userEntries[0].blocks.map((block: any) => block.text ?? "").join("") === "benchmark task", "historical user text");
    const assistantEntries = snapshot.entries.filter((entry: any) => entry.role === "assistant");
    let verified = 0;
    for (let entryIndex = 0; entryIndex < Math.ceil(count / workload.toolsPerAssistant); entryIndex++) {
        const entry = assistantEntries[entryIndex];
        invariant(entry?.messageId === `assistant-${entryIndex}`, `assistant ${entryIndex} order and identity`);
        invariant(entry.turnId === "turn:benchmark-user", `assistant ${entryIndex} turn ownership`);
        const text = entry.blocks.filter((block: any) => block.type === "text").map((block: any) => block.text).join("");
        invariant(text === "working\n", `assistant ${entryIndex} historical text`);
        const blocks = entry.blocks.filter((block: any) => block.type === "tool_call");
        invariant(blocks.length === Math.min(workload.toolsPerAssistant, count - verified), `assistant ${entryIndex} tool count`);
        for (const block of blocks) {
            const expected = tool(verified, large);
            const card = block.tool;
            invariant(card !== null && card !== undefined, `${expected.id} non-null public card`);
            invariant(block.toolCallId === expected.id && card.toolCallId === expected.id, `${expected.id} order and identity`);
            invariant(card.turnId === entry.turnId, `${expected.id} ownership`);
            invariant(card.name === "Read" && card.kind === "read", `${expected.id} metadata`);
            invariant(card.status === "completed", `${expected.id} completed status`);
            for (const field of ["arguments", "result"] as const) {
                const value = fieldValue(card, field, owner, reads);
                invariant(JSON.stringify(value) === JSON.stringify(field === "arguments" ? expected.argumentsValue : expected.resultValue),
                    `${expected.id} exact ${field}`);
            }
            verified++;
            if (verified % 256 === 0) budget.check("validate_public_history", verified);
        }
    }
    const totalTools = snapshot.entries.reduce((sum: number, entry: any) =>
        sum + entry.blocks.filter((block: any) => block.type === "tool_call").length, 0);
    invariant(verified === count && totalTools === count, "complete public tool history without duplicates");
    return { exact: true, verifiedTools: verified, verifiedAssistantMessages: Math.ceil(count / workload.toolsPerAssistant), ...reads };
}

export function publicText(api: any, replica: any): string {
    const view = api.readSessionView(replica.chat, replica.session);
    invariant(view.activeTurnStatus === "completed", "replica completed session");
    return view.entries.filter((entry: any) => entry.role === "assistant")
        .flatMap((entry: any) => entry.blocks).filter((block: any) => block.type === "text")
        .map((block: any) => block.text).join("");
}
