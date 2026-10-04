import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { assistant, byteLength, content, summarize, tool, update, user, workload } from "./workloads";
import { Budget, captureSnapshot, invariant, validateText, type Api, type Sample } from "./harness";
import { firstTool, publicText, validateHistory } from "./validation";

export async function projectTools(api: Api, result: Sample, budget: Budget, count: number = workload.tools, large = false) {
    const docs = new api.SessionDocs();
    docs.accept(user());
    let projectionMs = 0;
    let rawPayloadBytes = 0;
    const batches: number[] = [];
    let batchTime = 0;
    for (let index = 0; index < count; index++) {
        const value = tool(index, large);
        rawPayloadBytes += byteLength(value.argumentsValue) + byteLength(value.resultValue);
        const started = performance.now();
        if (index % workload.toolsPerAssistant === 0) docs.accept(assistant(Math.floor(index / workload.toolsPerAssistant)));
        docs.accept(value.start);
        docs.accept(value.finish);
        const elapsed = performance.now() - started;
        projectionMs += elapsed;
        batchTime += elapsed;
        if ((index + 1) % 1_000 === 0 || index + 1 === count) {
            batches.push(batchTime);
            batchTime = 0;
            result.projection = { completedTools: index + 1, rawPayloadBytes, projectionMs, batch1000Ms: [...batches] };
            budget.checkpoint("project_tools", index + 1);
        } else if (index % 64 === 0) budget.check("project_tools", index + 1);
    }
    result.projection = { completedTools: count, rawPayloadBytes, projectionMs, batch1000Ms: batches };
    return docs;
}

export async function toolsScenario(api: Api, result: Sample, budget: Budget, encoding: "v1" | "v2", large = false) {
    const Y = api.Y;
    const count = large ? 1 : workload.tools;
    const docs = await projectTools(api, result, budget, count, large);
    const completeStarted = performance.now();
    docs.completeTurn();
    result.completeMs = performance.now() - completeStarted;
    const replica = captureSnapshot(api, docs, result, budget, encoding);
    const view = api.readSessionView(replica.chat, replica.session);
    invariant(view.activeTurnStatus === "completed", "cold replica session completion");
    result.validation = { ...validateHistory(view, docs, count, large, budget), activeTurnStatus: view.activeTurnStatus };
    if (encoding === "v2") {
        const started = performance.now();
        const chat = Y.encodeStateAsUpdate(docs.chat), session = Y.encodeStateAsUpdate(docs.session);
        result.alternativeCodec = { encoding: "v1", chatBytes: chat.length, sessionBytes: session.length,
            totalBytes: chat.length + session.length, encodeMs: performance.now() - started,
            purpose: "same candidate documents encoded as V1 to separate hot payload isolation from codec" };
        budget.check("alternative_codec");
    }
    replica.chat.destroy(); replica.session.destroy(); docs.destroy();
}

export async function viewScenario(api: Api, result: Sample, budget: Budget, count: number = workload.tools, updates: number = workload.asyncUpdates) {
    const docs = await projectTools(api, result, budget, count);
    const initialStarted = performance.now();
    const store = new api.SessionViewStore(docs.chat, docs.session);
    result.initialViewMs = performance.now() - initialStarted;
    result.initialHistory = validateHistory(store.getSnapshot(), docs, count, false, budget);
    let snapshots = 0;
    let unchangedToolIdentity = 0;
    let missingToolSnapshots = 0;
    let previousTool = firstTool(store.getSnapshot());
    invariant(previousTool !== null, "initial first tool exists");
    store.subscribe(() => {
        snapshots++;
        const current = firstTool(store.getSnapshot());
        if (current === null) missingToolSnapshots++;
        if (current !== null && previousTool === current) unchangedToolIdentity++;
        previousTool = current;
    });
    const latencies: number[] = [];
    for (let index = 0; index < updates; index++) {
        const started = performance.now();
        docs.accept(assistant(Math.ceil(count / workload.toolsPerAssistant), content(index, workload.chunkBytes, "async")));
        await Promise.resolve();
        latencies.push(performance.now() - started);
        budget.check("async_view", index + 1);
    }
    invariant(snapshots === updates && missingToolSnapshots === 0, `view publishes ${snapshots} updates with non-null tools`);
    const expected = Array.from({ length: updates }, (_, i) => content(i, workload.chunkBytes, "async")).join("");
    const tail = store.getSnapshot().entries.at(-1).blocks.filter((block: any) => block.type === "text")
        .map((block: any) => block.text).join("");
    result.view = { snapshots, unchangedToolIdentity, latencyMs: summarize(latencies), rawLatencyMs: latencies };
    result.validation = { ...validateText(tail, expected), history: validateHistory(store.getSnapshot(), docs, count, false, budget) };
    const previousSnapshot = store.getSnapshot();
    const priorTool = firstTool(previousSnapshot);
    docs.accept(update("tool_call_update", { toolCallId: "tool-0", title: "Updated Read", status: "completed" }));
    await Promise.resolve();
    invariant(firstTool(store.getSnapshot())?.name === "Updated Read", "real tool update invalidates cache");
    invariant(firstTool(store.getSnapshot()) !== priorTool && priorTool.name === "Read", "prior tool snapshot is immutable");
    invariant(firstTool(previousSnapshot) === priorTool, "prior snapshot retains original tool");
    result.cacheContract = { changedToolInvalidated: true, priorSnapshotUnchanged: true,
        timing: "checked after the measured async update loop using one additional metadata update" };
    store.destroy(); docs.destroy();
}

export async function legacyStreamScenario(api: Api, root: string, result: Sample, budget: Budget) {
    const Y = api.Y;
    const { SessionDocStream } = await import(pathToFileURL(join(root, "examples/demo/session-doc-stream.ts")).href);
    const docs = new api.SessionDocs();
    const replica = { chat: new Y.Doc(), session: new Y.Doc() };
    const stream = new SessionDocStream(docs.chat, docs.session);
    let frames = 0, jsonBytes = 0, updateBytes = 0, applyMs = 0, projectionMs = 0;
    const connection = stream.subscribe((frame: any) => {
        frames++;
        jsonBytes += byteLength(frame);
        const bytes = Buffer.from(frame.update, "base64");
        updateBytes += bytes.length;
        const started = performance.now();
        Y.applyUpdate(replica[frame.doc as "chat" | "session"], bytes);
        applyMs += performance.now() - started;
    });
    const snapshotStarted = performance.now();
    Y.applyUpdate(replica.chat, Buffer.from(connection.snapshot.chat, "base64"));
    Y.applyUpdate(replica.session, Buffer.from(connection.snapshot.session, "base64"));
    result.initialSnapshot = { jsonBytes: byteLength(connection.snapshot), applyMs: performance.now() - snapshotStarted };
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
    result.completeMs = performance.now() - completeStarted;
    const endToEndMs = performance.now() - endToEndStarted;
    result.stream = { completedChunks: workload.streamChunks, frames, jsonBytes, updateBytes, applyMs, projectionMs,
        endToEndMs, completionFlushFrames: 0,
        timing: "endToEndMs includes input generation, user, all accepts, completion and synchronous replication; excludes initial snapshot and final validation",
        framing: "JSON event bodies; excludes SSE/HTTP/TCP overhead", projectionIncludesSynchronousTransportAndApply: true };
    const expected = Array.from({ length: workload.streamChunks }, (_, index) => content(index, workload.chunkBytes, "stream")).join("");
    result.validation = validateText(publicText(api, replica), expected);
    result.validation.activeTurnStatus = "completed";
    result.capabilities = { resume: "not supported by baseline demo bridge", slowConsumer: "not supported",
        versionedPayloadRead: "not supported", duplicateOutOfOrder: "not evaluated through bridge; Yjs primitive support is separate" };
    connection.unsubscribe(); stream.close(); docs.destroy(); replica.chat.destroy(); replica.session.destroy();
    budget.checkpoint("stream_verified", workload.streamChunks);
}
