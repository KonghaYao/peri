import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { pathToFileURL } from "node:url";
import { digest } from "./workloads";

export type Sample = Record<string, any>;
export type Api = Record<string, any>;

export async function loadApi(root: string, mode = "legacy"): Promise<Api> {
    const state = await import(pathToFileURL(join(root, "src/state/session-docs.ts")).href);
    const view = await import(pathToFileURL(join(root, "src/view/session-view.ts")).href);
    const sync = mode === "candidate" ? await import(pathToFileURL(join(root, "src/sync/index.ts")).href) : {};
    // Resolve Yjs from the tested root too: separately installed roots must not mix instanceof identities.
    const Y = await import(pathToFileURL(join(root, "node_modules/yjs/dist/yjs.mjs")).href);
    return { ...state, ...view, ...sync, Y };
}

export function sourceDigest(root: string): string {
    const hash = createHash("sha256");
    function visit(directory: string) {
        for (const entry of readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
            const file = join(directory, entry.name);
            if (entry.isDirectory()) visit(file);
            else hash.update(relative(root, file)).update("\0").update(readFileSync(file)).update("\0");
        }
    }
    visit(join(root, "src"));
    return hash.digest("hex");
}

export class Budget {
    readonly started = performance.now();
    private peakRss = process.memoryUsage.rss();
    constructor(readonly result: Sample, readonly timeoutMs: number, readonly maxRssBytes: number) {}

    check(stage: string, completed?: number): void {
        const rss = process.memoryUsage.rss();
        this.peakRss = Math.max(this.peakRss, rss, process.resourceUsage().maxRSS * 1024);
        this.result.progress = { stage, completed, elapsedMs: performance.now() - this.started };
        this.result.memory = { rssBytes: rss, peakRssBytes: this.peakRss, heapUsedBytes: process.memoryUsage().heapUsed };
        if (this.peakRss > this.maxRssBytes) throw new Error("RSS_BUDGET_EXCEEDED");
        if (performance.now() - this.started > this.timeoutMs) throw new Error("TIME_BUDGET_EXCEEDED");
    }

    checkpoint(stage: string, completed?: number): void {
        this.check(stage, completed);
        process.stdout.write(JSON.stringify({ progress: this.result.progress, memory: this.result.memory }) + "\n");
    }
}

export function invariant(condition: unknown, message: string): asserts condition {
    if (!condition) throw new Error(`VALIDATION_FAILED: ${message}`);
}

export function captureSnapshot(api: Api, docs: any, result: Sample, budget: Budget, encoding: "v1" | "v2") {
    const Y = api.Y;
    budget.checkpoint("snapshot_encode");
    const encode = encoding === "v2" ? Y.encodeStateAsUpdateV2 : Y.encodeStateAsUpdate;
    const apply = encoding === "v2" ? Y.applyUpdateV2 : Y.applyUpdate;
    const started = performance.now();
    const chat = encode(docs.chat);
    const session = encode(docs.session);
    result.snapshot = { encoding, chatBytes: chat.length, sessionBytes: session.length,
        totalBytes: chat.length + session.length, encodeMs: performance.now() - started };
    budget.checkpoint("snapshot_apply");
    const replica = { chat: new Y.Doc(), session: new Y.Doc() };
    const applyStarted = performance.now();
    apply(replica.chat, chat);
    apply(replica.session, session);
    result.snapshot.applyMs = performance.now() - applyStarted;
    budget.check("snapshot_verified");
    return replica;
}

export function validateText(actual: string, expected: string) {
    const actualSha256 = digest(actual), expectedSha256 = digest(expected);
    invariant(actual.length === expected.length && actualSha256 === expectedSha256, "text length and SHA-256");
    return { bytes: Buffer.byteLength(actual), sha256: actualSha256, expectedSha256, exact: true };
}
