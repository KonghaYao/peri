import { describe, expect, test } from "bun:test";
import {
    PayloadReadError, VIEW_PAYLOAD_MAX_BYTES, VIEW_PAYLOAD_MAX_CONCURRENCY, limitPayloadReads, readPayload,
    type ToolPayloadReader,
} from "../src/view/payload-reader";

const encoder = new TextEncoder();
const response = (id: string, json: string) => ({ id, bytes: encoder.encode(json).byteLength, json });

describe("browser payload reader contract", () => {
    test("frozen defaults match the approved review starting points", () => {
        expect(VIEW_PAYLOAD_MAX_BYTES).toBe(1_048_576);
        expect(VIEW_PAYLOAD_MAX_CONCURRENCY).toBe(2);
    });

    test("validated read returns the exact public JSON for the referenced version", async () => {
        const json = JSON.stringify({ contentText: "结果", rawOutput: { lines: 2 } });
        const ref = { id: "p1", bytes: encoder.encode(json).byteLength };
        const seen: string[] = [];
        const reader: ToolPayloadReader = async (request) => { seen.push(request.id); return response("p1", json); };
        await expect(readPayload(ref, reader)).resolves.toEqual({ id: "p1", bytes: ref.bytes, json });
        expect(seen).toEqual(["p1"]);
    });

    test("a reference beyond the display budget fails before any read", async () => {
        let called = false;
        const reader: ToolPayloadReader = async () => { called = true; return response("p1", "{}"); };
        const failure = await readPayload({ id: "p1", bytes: VIEW_PAYLOAD_MAX_BYTES + 1 }, reader).catch((error) => error);
        expect(failure).toBeInstanceOf(PayloadReadError);
        expect((failure as PayloadReadError).code).toBe("too_large");
        expect(called).toBe(false);
        await readPayload({ id: "p1", bytes: 3 }, reader, { maxBytes: 2 }).catch((error) => {
            expect((error as PayloadReadError).code).toBe("too_large");
        });
    });

    test("id, byte length and JSON validity are all enforced as integrity errors", async () => {
        const ref = { id: "p1", bytes: 7 };
        const cases: Array<[ToolPayloadReader, string]> = [
            [async () => response("other", "{}    "), "id"],
            [async () => response("p1", "{}"), "bytes"],
            [async () => ({ bytes: ref.bytes, json: "1234567" }) as never, "shape"],
            [async () => response("p1", "not json"), "json"],
        ];
        for (const [reader, label] of cases) {
            const failure = await readPayload(ref, reader).catch((error) => error);
            expect(failure, label).toBeInstanceOf(PayloadReadError);
            expect((failure as PayloadReadError).code, label).toBe("integrity_error");
        }
    });

    test("abort, reader failures and typed reader errors keep their identity", async () => {
        const ref = { id: "p1", bytes: 2 };
        const controller = new AbortController();
        controller.abort();
        const preAborted = await readPayload(ref, async () => response("p1", "{}"), { signal: controller.signal }).catch((error) => error);
        expect((preAborted as PayloadReadError).code).toBe("aborted");

        const aborting = new AbortController();
        const aborted = await readPayload(ref, (request) => {
            aborting.abort();
            return Promise.reject(new Error(`for ${request.id}`));
        }, { signal: aborting.signal }).catch((error) => error);
        expect((aborted as PayloadReadError).code).toBe("aborted");

        const transport = await readPayload(ref, async () => { throw new Error("socket closed"); }).catch((error) => error);
        expect((transport as PayloadReadError).code).toBe("transport_error");
        expect(((transport as PayloadReadError).cause as Error).message).toBe("socket closed");

        const unauthorized = await readPayload(ref, async () => { throw new PayloadReadError("unauthorized", "401"); }).catch((error) => error);
        expect((unauthorized as PayloadReadError).code).toBe("unauthorized");
        const unavailable = await readPayload(ref, async () => { throw new PayloadReadError("unavailable", "stale"); }).catch((error) => error);
        expect((unavailable as PayloadReadError).code).toBe("unavailable");
    });
});

describe("payload read concurrency gate", () => {
    test("at most two reads are in flight and the rest queue in FIFO order", async () => {
        let active = 0;
        let peak = 0;
        const started: string[] = [];
        const releases: Array<() => void> = [];
        const reader: ToolPayloadReader = (request) => new Promise((resolve) => {
            active++; peak = Math.max(peak, active); started.push(request.id);
            releases.push(() => { active--; resolve(response(request.id, "{}")); });
        });
        const limited = limitPayloadReads(reader);
        const reads = ["a", "b", "c", "d"].map((id) => limited({ id }));
        const until = async (check: () => boolean, label: string) => {
            for (let index = 0; index < 100 && !check(); index++) await Promise.resolve();
            expect(check(), label).toBe(true);
        };
        await until(() => started.length === 2, "only two reads start");
        expect(started).toEqual(["a", "b"]);
        releases.shift()!();
        await until(() => started.length === 3, "one queued read starts after a release");
        expect(started).toEqual(["a", "b", "c"]);
        releases.shift()!();
        await until(() => started.length === 4, "the last queued read starts");
        expect(started).toEqual(["a", "b", "c", "d"]);
        releases.shift()!();
        releases.shift()!();
        await Promise.all(reads);
        expect(peak).toBe(2);
    });

    test("a failing read releases its slot for the queued reader", async () => {
        let calls = 0;
        const limited = limitPayloadReads(async (request) => {
            calls++;
            if (request.id === "bad") throw new PayloadReadError("unavailable", "stale");
            return response(request.id, "{}");
        }, 1);
        const failing = limited({ id: "bad" }).catch((error) => error);
        const queued = limited({ id: "good" });
        expect((await failing as PayloadReadError).code).toBe("unavailable");
        expect((await queued).id).toBe("good");
        expect(calls).toBe(2);
    });
});
