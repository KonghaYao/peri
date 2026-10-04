import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { assistant, byteLength, content, workload } from "./workloads";
import { Budget, invariant, validateText, type Api, type Sample } from "./harness";
import { projectTools } from "./scenarios";
import { firstTool, validateHistory } from "./validation";

const options = { flushIntervalMs: 16, maxBatchBytes: 64 * 1024, maxBatchUpdates: 256, maxSubscriberBytes: 4 * 1024 * 1024 };

/** Snapshot byte accounting avoids retaining a second giant JSON string just for measurement. */
function snapshotJsonBytes(snapshot: any): number {
    const size = (value: string | Uint8Array) => typeof value === "string" ? Buffer.byteLength(value) : 4 * Math.ceil(value.byteLength / 3);
    return size(snapshot.chat) + size(snapshot.session) + byteLength({ ...snapshot, chat: "", session: "" });
}

async function connect(api: Api, docs: any, root: string, mode: string, result: Sample, counters: any, budget: Budget) {
    const Y = api.Y;
    const snapshotStarted = performance.now();
    if (mode === "legacy") {
        const { SessionDocStream } = await import(pathToFileURL(join(root, "examples/demo/session-doc-stream.ts")).href);
        const stream = new SessionDocStream(docs.chat, docs.session);
        const replica = { chat: new Y.Doc(), session: new Y.Doc() };
        const connection = stream.subscribe((frame: any) => {
            counters.frames++;
            counters.jsonBytes += byteLength(frame);
            const bytes = Buffer.from(frame.update, "base64");
            counters.updateBytes += bytes.byteLength;
            const started = performance.now();
            Y.applyUpdate(replica[frame.doc as "chat" | "session"], bytes);
            counters.applyMs += performance.now() - started;
        });
        result.initialSnapshot = { encoding: "v1", jsonBytes: snapshotJsonBytes(connection.snapshot),
            updateBytes: Buffer.byteLength(connection.snapshot.chat, "base64") + Buffer.byteLength(connection.snapshot.session, "base64") };
        const applyStarted = performance.now();
        Y.applyUpdate(replica.chat, Buffer.from(connection.snapshot.chat, "base64"));
        Y.applyUpdate(replica.session, Buffer.from(connection.snapshot.session, "base64"));
        result.initialSnapshot.applyMs = performance.now() - applyStarted;
        result.initialSnapshot.totalMs = performance.now() - snapshotStarted;
        budget.checkpoint("combined_snapshot_applied");
        const unsubscribe = connection.unsubscribe;
        return { replica, flush: () => {}, close: async () => { stream.close(); },
            destroy: () => { unsubscribe(); replica.chat.destroy(); replica.session.destroy(); } };
    }
    const { SessionDocStream } = await import(pathToFileURL(join(root, "examples/demo/session-doc-stream.ts")).href);
    const sync = new SessionDocStream(docs.chat, docs.session);
    const replica = new api.SessionDocReplica();
    let error: Error | undefined;
    const connection = sync.subscribe((frame: any) => {
        counters.frames++;
        counters.jsonBytes += byteLength(frame);
        const binary = { ...frame,
            ...(frame.chat ? { chat: Buffer.from(frame.chat, "base64") } : {}),
            ...(frame.session ? { session: Buffer.from(frame.session, "base64") } : {}) };
        counters.updateBytes += (binary.chat?.byteLength ?? 0) + (binary.session?.byteLength ?? 0);
        const started = performance.now();
        replica.applyUpdate(binary);
        counters.applyMs += performance.now() - started;
    }, { onError: (failure: Error) => { error = failure; } });
    result.initialSnapshot = { encoding: "v2", jsonBytes: snapshotJsonBytes(connection.snapshot),
        updateBytes: Buffer.byteLength(connection.snapshot.chat, "base64") + Buffer.byteLength(connection.snapshot.session, "base64") };
    const applyStarted = performance.now();
    replica.applySnapshot({ ...connection.snapshot, chat: Buffer.from(connection.snapshot.chat, "base64"),
        session: Buffer.from(connection.snapshot.session, "base64") });
    result.initialSnapshot.applyMs = performance.now() - applyStarted;
    result.initialSnapshot.totalMs = performance.now() - snapshotStarted;
    budget.checkpoint("combined_snapshot_applied");
    const unsubscribe = connection.unsubscribe;
    return { replica, flush: () => sync.flush(), close: async () => { await sync.close(); invariant(!error, `sync failed: ${error?.message}`); },
        destroy: () => { unsubscribe(); replica.destroy(); } };
}

export async function combinedScenario(api: Api, root: string, mode: string, result: Sample, budget: Budget) {
    const docs = await projectTools(api, result, budget);
    const counters = { frames: 0, jsonBytes: 0, updateBytes: 0, applyMs: 0 };
    const link = await connect(api, docs, root, mode, result, counters, budget);
    const store = new api.SessionViewStore(link.replica.chat, link.replica.session);
    result.initialHistory = validateHistory(store.getSnapshot(), docs, workload.tools, false, budget);
    let publications = 0, unchangedToolIdentity = 0, missingToolSnapshots = 0;
    let previousTool = firstTool(store.getSnapshot());
    invariant(previousTool !== null, "initial combined tool exists");
    store.subscribe(() => {
        publications++;
        const current = firstTool(store.getSnapshot());
        if (current === null) missingToolSnapshots++;
        if (current !== null && current === previousTool) unchangedToolIdentity++;
        previousTool = current;
    });
    let projectionMs = 0;
    const started = performance.now();
    const messageIndex = workload.tools / workload.toolsPerAssistant;
    for (let index = 0; index < workload.streamChunks; index++) {
        const text = content(index, workload.chunkBytes, "stream");
        const acceptStarted = performance.now();
        docs.accept(assistant(messageIndex, text));
        projectionMs += performance.now() - acceptStarted;
        if ((index + 1) % 64 === 0) {
            link.flush();
            await Promise.resolve();
            result.stream = { ...counters, completedChunks: index + 1, projectionMs, publications, unchangedToolIdentity,
                endToEndMs: performance.now() - started };
            if ((index + 1) % 2_048 === 0) budget.checkpoint("combined_stream", index + 1);
            else budget.check("combined_stream", index + 1);
        }
    }
    const beforeCompletion = counters.frames;
    docs.completeTurn();
    link.flush();
    await link.close();
    await Promise.resolve();
    result.stream = { ...counters, completedChunks: workload.streamChunks, projectionMs, publications, unchangedToolIdentity,
        endToEndMs: performance.now() - started, completionFrames: counters.frames - beforeCompletion,
        yieldEveryChunks: 64, options: mode === "candidate" ? options : null,
        timing: "from first stream chunk through completion, flush, close, replica apply and view microtasks; excludes history seed, initial snapshot and full content validation",
        framing: "actual demo SSE adapter encode/decode, incremental doc payload bytes and JSON bodies; initial snapshot separately recorded; excludes HTTP/SSE/TCP" };
    budget.checkpoint("combined_stream_completed", workload.streamChunks);
    invariant(publications === workload.streamChunks / 64 + 1, `expected 257 replica view publications, got ${publications}`);
    invariant(missingToolSnapshots === 0, "tools retained during every publication");
    const final = store.getSnapshot();
    invariant(final.activeTurnStatus === "completed", "combined replica completion");
    const streamEntries = final.entries.filter((entry: any) => entry.messageId === `assistant-${messageIndex}`);
    invariant(streamEntries.length === 1, "one complete stream message after tool history");
    const actual = streamEntries[0].blocks.filter((block: any) => block.type === "text").map((block: any) => block.text).join("");
    const expected = Array.from({ length: workload.streamChunks }, (_, index) => content(index, workload.chunkBytes, "stream")).join("");
    result.validation = { ...validateText(actual, expected), activeTurnStatus: "completed",
        history: validateHistory(final, docs, workload.tools, false, budget) };
    store.destroy(); link.destroy(); docs.destroy();
}
