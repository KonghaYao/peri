import { assistant, byteLength, content, user, workload } from "./workloads";
import { Budget, invariant, validateText, type Api, type Sample } from "./harness";
import { publicText } from "./validation";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

function sseBody(frame: any) {
    return { protocol: frame.protocol, generation: frame.generation, sequence: frame.sequence,
        ...(frame.mode ? { mode: frame.mode } : {}),
        ...(frame.chat ? { chat: Buffer.from(frame.chat).toString("base64") } : {}),
        ...(frame.session ? { session: Buffer.from(frame.session).toString("base64") } : {}) };
}

export async function candidateStreamScenario(api: Api, result: Sample, budget: Budget, sseRoot?: string) {
    const docs = new api.SessionDocs();
    const replica = new api.SessionDocReplica();
    const options = { flushIntervalMs: 16, maxBatchBytes: 64 * 1024, maxBatchUpdates: 256, maxSubscriberBytes: 4 * 1024 * 1024 };
    const Adapter = sseRoot ? (await import(pathToFileURL(join(sseRoot, "examples/demo/session-doc-stream.ts")).href)).SessionDocStream : null;
    const sync = Adapter ? new Adapter(docs.chat, docs.session) : new api.SessionDocSync(docs.chat, docs.session, options);
    let frames = 0, jsonBytes = 0, updateBytes = 0, applyMs = 0, projectionMs = 0;
    let flushCalls = 0, nonEmptyFlushCalls = 0;
    let transportError: Error | undefined;
    const flush = sync.flush.bind(sync);
    sync.flush = () => {
        const before = frames;
        flushCalls++;
        flush();
        if (frames > before) nonEmptyFlushCalls++;
    };
    const connection = sync.subscribe((frame: any) => {
        frames++;
        jsonBytes += byteLength(sseRoot ? frame : sseBody(frame));
        const binary = sseRoot ? { ...frame,
            ...(frame.chat ? { chat: Buffer.from(frame.chat, "base64") } : {}),
            ...(frame.session ? { session: Buffer.from(frame.session, "base64") } : {}) } : frame;
        updateBytes += (binary.chat?.byteLength ?? 0) + (binary.session?.byteLength ?? 0);
        const started = performance.now();
        replica.applyUpdate(binary);
        applyMs += performance.now() - started;
    }, { onError: (error: Error) => { transportError = error; } });
    const snapshotStarted = performance.now();
    const initial = sseRoot ? { ...connection.snapshot, chat: Buffer.from(connection.snapshot.chat, "base64"),
        session: Buffer.from(connection.snapshot.session, "base64") } : connection.snapshot;
    replica.applySnapshot(initial);
    result.initialSnapshot = { encoding: "v2", mode: connection.snapshot.mode,
        updateBytes: initial.chat.length + initial.session.length,
        jsonBytes: byteLength(sseRoot ? connection.snapshot : sseBody(connection.snapshot)), applyMs: performance.now() - snapshotStarted };
    flushCalls = 0; nonEmptyFlushCalls = 0;
    const endToEndStarted = performance.now();
    docs.accept(user());
    for (let index = 0; index < workload.streamChunks; index++) {
        const text = content(index, workload.chunkBytes, "stream");
        const started = performance.now();
        docs.accept(assistant(0, text));
        projectionMs += performance.now() - started;
        if ((index + 1) % 2_048 === 0) {
            result.stream = { completedChunks: index + 1, frames, jsonBytes, updateBytes, applyMs, projectionMs };
            budget.checkpoint("stream", index + 1);
        } else if (index % 64 === 0) budget.check("stream", index + 1);
    }
    const completeStarted = performance.now();
    docs.completeTurn();
    const beforeCompletionFlush = frames;
    sync.flush();
    await sync.close();
    result.completeMs = performance.now() - completeStarted;
    const endToEndMs = performance.now() - endToEndStarted;
    invariant(!transportError, `candidate transport error: ${transportError?.message}`);
    result.stream = { completedChunks: workload.streamChunks, encoding: "v2", frames, jsonBytes, updateBytes,
        applyMs, projectionMs, endToEndMs, options,
        transport: sseRoot ? "demo SSE adapter with actual base64 encode/decode" : "native binary plus equivalent SSE byte accounting",
        flushCalls: sseRoot ? undefined : flushCalls, nonEmptyFlushCalls: sseRoot ? frames : nonEmptyFlushCalls,
        explicitFlushCalls: sseRoot ? 1 : undefined,
        completionFlushFrames: frames - beforeCompletionFlush,
        framing: "native binary doc payloads plus equivalent demo SSE JSON/base64 body bytes; excludes transport headers and undefined binary envelope serialization",
        projectionIncludesSynchronousTransportAndApply: "only threshold-triggered synchronous flushes; deferred tail accounted in endToEndMs",
        timing: "endToEndMs includes input generation, user, all accepts, completion, explicit flush, close and replica apply; excludes initial snapshot and final validation" };
    const expected = Array.from({ length: workload.streamChunks }, (_, index) => content(index, workload.chunkBytes, "stream")).join("");
    result.validation = { ...validateText(publicText(api, replica), expected), activeTurnStatus: "completed" };
    connection.unsubscribe(); docs.destroy(); replica.destroy();
    budget.checkpoint("stream_verified", workload.streamChunks);
}
