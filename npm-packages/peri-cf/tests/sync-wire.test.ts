import { afterEach, describe, expect, test } from "bun:test";
import { SessionDocs, SessionDocSync, decodeAuthFrame, decodeSyncFrame, encodeAuthFrame, encodeSyncFrame,
  decodeAckFrame, encodeAckFrame, FRAME_ACK, FRAME_UPDATE, SYNC_WIRE_VERSION,
  MAX_ACK_FRAME_BYTES, MAX_AUTH_FRAME_BYTES, MAX_SYNC_FRAME_BYTES, type SyncFramePayload } from "../worker/sdk";
import { SessionDocReplica, SessionViewStore } from "@peri-code/sdk/view";
import { readSyncState } from "../shared/sync-state";

const cleanups: (() => void | Promise<void>)[] = [];
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
});

function pair() {
  const docs = new SessionDocs();
  const sync = new SessionDocSync(docs.chat, docs.session);
  const replica = new SessionDocReplica();
  cleanups.push(() => docs.destroy(), () => sync.close(), () => replica.destroy());
  return { docs, sync, replica };
}

let delivery = 0;
function deliver(replica: SessionDocReplica, frame: SyncFramePayload) {
  const decoded = decodeSyncFrame(encodeSyncFrame({ ...frame, delivery: ++delivery }));
  if (decoded.type === "snapshot") replica.applySnapshot(decoded.snapshot);
  else replica.applyUpdate(decoded.update);
}

function chunk(docs: SessionDocs, text: string) {
  docs.accept({ jsonrpc: "2.0", method: "session/update", params: {
    sessionId: "fixture", update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } },
  } });
}

describe("SDK document sync wire boundary", () => {
  test.each([0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1])("rejects invalid ACK delivery %s", (value) => {
    expect(() => encodeAckFrame(value)).toThrow();
  });

  test("rejects a zero delivery inside a well-formed ACK frame", () => {
    expect(() => decodeAckFrame(new Uint8Array([FRAME_ACK, SYNC_WIRE_VERSION, 0]))).toThrow();
  });

  test("ACK frames cannot carry credentials, writable document fields or trailing bytes", () => {
    const ack = encodeAckFrame(1);
    const writable = encodeSyncFrame({ type: "update", delivery: 1, update: {
      protocol: 2, generation: "fixture", sequence: 0, chat: new Uint8Array([1]),
    } });
    for (const raw of [encodeAuthFrame({ type: "auth", token: "secret" }), writable, new Uint8Array([...ack, 0]),
      new Uint8Array(MAX_ACK_FRAME_BYTES + 1), '{"type":"ack","delivery":1}', " ".repeat(129), new Uint8Array()])
      expect(() => decodeAckFrame(raw)).toThrow();
    expect(decodeAckFrame(ack)).toEqual({ type: "ack", delivery: 1 });
  });

  test("frames use an exact binary layout with tag, wire version and varuint delivery", () => {
    expect(Array.from(encodeAckFrame(300))).toEqual([FRAME_ACK, SYNC_WIRE_VERSION, 0xac, 0x02]);
    expect(Array.from(encodeAuthFrame({ type: "auth", token: "t" }))).toEqual([0x01, SYNC_WIRE_VERSION, 1, 0x74, 0]);
    const snapshot = encodeSyncFrame({ type: "snapshot", delivery: 300, snapshot: { protocol: 2, generation: "g1",
      sequence: 7, mode: "delta", chat: new Uint8Array([1, 2]), session: new Uint8Array([3]) } });
    expect(Array.from(snapshot)).toEqual([0x02, SYNC_WIRE_VERSION, 0xac, 0x02, 2, 2, 0x67, 0x31, 7, 1, 2, 1, 2, 1, 3]);
    expect(decodeSyncFrame(snapshot)).toEqual({ type: "snapshot", delivery: 300, snapshot: { protocol: 2,
      generation: "g1", sequence: 7, mode: "delta", chat: new Uint8Array([1, 2]), session: new Uint8Array([3]) } });
    const update = encodeSyncFrame({ type: "update", delivery: 1, update: { protocol: 2, generation: "g1",
      sequence: 0, session: new Uint8Array([9]) } });
    expect(Array.from(update)).toEqual([0x03, SYNC_WIRE_VERSION, 1, 2, 2, 0x67, 0x31, 0, 0b10, 1, 9]);
  });

  test("preserves transport delivery identity independently of SDK generation and sequence", () => {
    const raw = encodeSyncFrame({ type: "update", delivery: 3, update: {
      protocol: 2, generation: "fixture", sequence: 1, chat: new Uint8Array([0]),
    } });
    expect(decodeSyncFrame(raw)).toMatchObject({ delivery: 3, update: { generation: "fixture", sequence: 1 } });
    expect(decodeAckFrame(encodeAckFrame(3))).toEqual({ type: "ack", delivery: 3 });
  });
  test("projects ACP through real SessionDocs and replicates SDK view without text accumulation", async () => {
    const { docs, sync, replica } = pair();
    const connection = sync.subscribe((update) => deliver(replica, { type: "update", update }));
    cleanups.push(connection.unsubscribe);
    deliver(replica, { type: "snapshot", snapshot: connection.snapshot });
    const view = new SessionViewStore(replica.chat, replica.session);
    cleanups.push(() => view.destroy());
    docs.acceptDeliveredUserInput("request-1", "Explain Yjs");
    chunk(docs, "你好 ");
    chunk(docs, "Yjs 🌍");
    docs.completeTurn("completed");
    sync.flush();
    await Promise.resolve();
    const entries = view.getSnapshot().entries;
    expect(entries.map((entry) => entry.role)).toEqual(["user", "assistant"]);
    expect(entries[1]?.blocks).toContainEqual(expect.objectContaining({ type: "text", text: "你好 Yjs 🌍" }));
    expect(entries[1]?.status).toBe("completed");
  });

  test("auth transports a state vector and SDK supplies missing deltas on reconnect", () => {
    const { docs, sync, replica } = pair();
    docs.acceptDeliveredUserInput("request-1", "Prompt");
    const first = sync.subscribe(() => {});
    deliver(replica, { type: "snapshot", snapshot: first.snapshot });
    first.unsubscribe();
    chunk(docs, "While disconnected");
    docs.completeTurn("cancelled");
    const auth = decodeAuthFrame(encodeAuthFrame({ type: "auth", token: "fixture-only", resume: replica.stateVector() }));
    expect(auth.resume?.chat).toBeInstanceOf(Uint8Array);
    const resumed = sync.subscribe(() => {}, { resume: auth.resume });
    cleanups.push(resumed.unsubscribe);
    expect(resumed.snapshot.mode).toBe("delta");
    deliver(replica, { type: "snapshot", snapshot: resumed.snapshot });
    const view = new SessionViewStore(replica.chat, replica.session);
    cleanups.push(() => view.destroy());
    expect(view.getSnapshot().entries[1]?.status).toBe("cancelled");
    expect(view.getSnapshot().entries[1]?.blocks[0]).toEqual(expect.objectContaining({ text: "While disconnected" }));
  });

  test("new producer generation requires snapshot replacement instead of merging old history", () => {
    const first = pair();
    const second = pair();
    first.docs.acceptDeliveredUserInput("old", "Old private prompt");
    const old = first.sync.subscribe(() => {});
    deliver(first.replica, { type: "snapshot", snapshot: old.snapshot });
    old.unsubscribe();
    second.docs.acceptDeliveredUserInput("new", "New prompt");
    const current = second.sync.subscribe(() => {}, { resume: first.replica.stateVector() });
    cleanups.push(current.unsubscribe);
    expect(current.snapshot.mode).toBe("snapshot");
    deliver(first.replica, { type: "snapshot", snapshot: current.snapshot });
    const view = new SessionViewStore(first.replica.chat, first.replica.session);
    cleanups.push(() => view.destroy());
    expect(JSON.stringify(view.getSnapshot())).not.toContain("Old private prompt");
    expect(JSON.stringify(view.getSnapshot())).toContain("New prompt");
  });

  test("rejects update sequence gaps then repairs from the last accepted vector", () => {
    const { docs, sync, replica } = pair();
    const updates: SyncFramePayload[] = [];
    const connection = sync.subscribe((update) => { updates.push({ type: "update", update }); });
    deliver(replica, { type: "snapshot", snapshot: connection.snapshot });
    docs.acceptDeliveredUserInput("request-1", "Prompt");
    sync.flush();
    chunk(docs, "Reply");
    sync.flush();
    expect(updates.length).toBe(2);
    expect(() => deliver(replica, updates[1]!)).toThrow("sequence gap");
    connection.unsubscribe();
    const resumed = sync.subscribe(() => {}, { resume: replica.stateVector() });
    cleanups.push(resumed.unsubscribe);
    deliver(replica, { type: "snapshot", snapshot: resumed.snapshot });
    expect(replica.stateVector()?.generation).toBe(sync.generation);
  });

  test("preserves UI metadata as a read-only synchronized document projection", () => {
    const { docs, sync, replica } = pair();
    docs.acceptDeliveredUserInput("request-1", "Prompt");
    chunk(docs, "Reply");
    const state = { chat: { id: "fixture", title: "Title", updatedAt: "now" },
      running: true, executionBlocked: false, entryMetadata: {
        "turn:request-1:user": { id: "stable-user", createdAt: "timestamp" },
        "turn:request-1:assistant": { id: "stable-assistant", createdAt: "timestamp" },
      } };
    docs.session.getMap("peri-cf").set("state", state);
    const connection = sync.subscribe(() => {});
    cleanups.push(connection.unsubscribe);
    deliver(replica, { type: "snapshot", snapshot: connection.snapshot });
    const projected = readSyncState(replica.chat, replica.session);
    expect(projected.chat).toEqual(state.chat);
    expect(projected.messages.map((message) => message.content)).toEqual(["Prompt", "Reply"]);
    expect(projected.messages.map((message) => message.id)).toEqual(["stable-user", "stable-assistant"]);
    expect(projected.messages[1]?.status).toBe("running");
    expect(replica.session.getMap("peri-cf").get("state")).not.toHaveProperty("messages");
  });

  test("application settlement failures remain errors even after the SDK turn completed", () => {
    const { docs } = pair();
    docs.acceptDeliveredUserInput("request-1", "Prompt");
    chunk(docs, "Completed model output");
    docs.completeTurn("completed");
    docs.session.getMap("peri-cf").set("state", {
      chat: { id: "fixture", title: "Title", updatedAt: "now" },
      running: false, executionBlocked: true, entryMetadata: {
        "turn:request-1:assistant": { id: "assistant", createdAt: "now", error: "Host exit unconfirmed" },
      },
    });
    const state = readSyncState(docs.chat, docs.session);
    expect(state.messages[1]?.status).toBe("error");
    expect(state.messages[1]?.content).toBe("Completed model output");
    expect(state.executionBlocked).toBe(true);
  });

  test.each(["not json", "null", "[]", '{"type":"write","update":"AA=="}',
    '{"type":"auth","token":"fixture","update":"AA=="}'])
    ("rejects non-auth or writable client frame %s", (raw) => {
      expect(() => decodeAuthFrame(raw)).toThrow();
    });

  test("accepts no text frames at all", () => {
    const frame = encodeSyncFrame({ type: "update", delivery: 1, update: {
      protocol: 2, generation: "fixture", sequence: 0, chat: new Uint8Array([1]),
    } });
    for (const raw of [new TextDecoder().decode(frame), JSON.stringify({ type: "update", chat: "AA==" })])
      expect(() => decodeSyncFrame(raw)).toThrow();
  });

  test("bounds raw frames before decoding", () => {
    expect(() => decodeAuthFrame(new Uint8Array(MAX_AUTH_FRAME_BYTES + 1))).toThrow();
    expect(() => decodeSyncFrame(new Uint8Array(MAX_SYNC_FRAME_BYTES + 1))).toThrow();
    expect(() => decodeSyncFrame(new Uint8Array())).toThrow();
    expect(() => decodeSyncFrame(new Uint8Array([FRAME_UPDATE]))).toThrow();
  });

  test("rejects truncated, unknown, malformed and trailing frame bytes", () => {
    const frame = encodeSyncFrame({ type: "update", delivery: 1, update: {
      protocol: 2, generation: "fixture", sequence: 0, chat: new Uint8Array([7]),
    } });
    const unknownTag = new Uint8Array(frame);
    unknownTag[0] = 0x09;
    const unknownVersion = new Uint8Array(frame);
    unknownVersion[1] = SYNC_WIRE_VERSION + 1;
    const invalidUtf8 = new Uint8Array([FRAME_UPDATE, SYNC_WIRE_VERSION, 1, 2, 2, 0xc3, 0x28, 0, 1, 1, 0]);
    const emptyMask = new Uint8Array([FRAME_UPDATE, SYNC_WIRE_VERSION, 1, 2, 2, 0x67, 0x31, 0, 0]);
    const unknownMask = new Uint8Array([FRAME_UPDATE, SYNC_WIRE_VERSION, 1, 2, 2, 0x67, 0x31, 0, 0b100]);
    const missingDocument = new Uint8Array([FRAME_UPDATE, SYNC_WIRE_VERSION, 1, 2, 2, 0x67, 0x31, 0, 0b01]);
    const zeroDelivery = new Uint8Array([FRAME_UPDATE, SYNC_WIRE_VERSION, 0, 2, 2, 0x67, 0x31, 0, 0b01, 1, 7]);
    const lengthBeyondFrame = new Uint8Array([FRAME_UPDATE, SYNC_WIRE_VERSION, 1, 2, 2, 0x67, 0x31, 0, 0b01, 9, 7]);
    for (const raw of [frame.subarray(0, frame.byteLength - 1), new Uint8Array([...frame, 0]), unknownTag,
      unknownVersion, invalidUtf8, emptyMask, unknownMask, missingDocument, zeroDelivery, lengthBeyondFrame])
      expect(() => decodeSyncFrame(raw)).toThrow();
  });

  test("decodes ArrayBuffer input and views with a non-zero byte offset", () => {
    const frame = encodeSyncFrame({ type: "snapshot", delivery: 4, snapshot: { protocol: 2, generation: "fixture",
      sequence: 2, mode: "snapshot", chat: new Uint8Array([5]), session: new Uint8Array([6]) } });
    const padded = new Uint8Array(frame.byteLength + 4);
    padded.set(frame, 2);
    const view = new Uint8Array(padded.buffer, 2, frame.byteLength);
    expect(decodeSyncFrame(view)).toEqual(decodeSyncFrame(frame));
    expect(decodeSyncFrame(padded.buffer.slice(2, 2 + frame.byteLength))).toEqual(decodeSyncFrame(frame));
    expect(decodeSyncFrame(frame)).toMatchObject({ type: "snapshot", delivery: 4 });
  });

  test("rejects oversized resume vectors, oversized documents and empty updates", () => {
    expect(() => encodeAuthFrame({ type: "auth", token: "fixture", resume: {
      protocol: 2, generation: "fixture", chat: new Uint8Array(64 * 1024 + 1), session: new Uint8Array(),
    } })).toThrow();
    expect(() => encodeSyncFrame({ type: "snapshot", delivery: 1, snapshot: { protocol: 2, generation: "fixture",
      sequence: 0, mode: "snapshot", chat: new Uint8Array(4 * 1024 * 1024 + 1), session: new Uint8Array(),
    } })).toThrow();
    expect(() => encodeSyncFrame({ type: "update", delivery: 1, update: {
      protocol: 2, generation: "fixture", sequence: 1,
    } })).toThrow();
  });
});
