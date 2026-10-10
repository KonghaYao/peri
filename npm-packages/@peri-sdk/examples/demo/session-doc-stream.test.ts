import { expect, test } from "bun:test";
import * as Y from "yjs";
import { Hono } from "hono";
import { streamSSE } from "hono/streaming";
import { SessionDocStream, decodeResume } from "./session-doc-stream";
import { streamSessionDocuments } from "./session-sse";
import { SessionEventLog } from "./session-event-log";
import { SessionDocReplica } from "../../src/sync/index";

const decode = (value: string) => Uint8Array.from(Buffer.from(value, "base64"));
const snapshot = (value: any) => ({ ...value, chat: decode(value.chat), session: decode(value.session) });
const update = (value: any) => ({ ...value, ...(value.chat ? { chat: decode(value.chat) } : {}),
  ...(value.session ? { session: decode(value.session) } : {}) });

test("SSE adapter batches V2 updates and resumes an existing document pair", async () => {
  const chat = new Y.Doc(); const session = new Y.Doc();
  chat.getMap("root").set("message", "history");
  const bridge = new SessionDocStream(chat, session);
  const replica = new SessionDocReplica();
  const frames: any[] = [];
  const connection = bridge.subscribe((frame) => { frames.push(frame); replica.applyUpdate(update(frame)); });
  replica.applySnapshot(snapshot(connection.snapshot));
  chat.getMap("root").set("message", "live"); session.getMap("root").set("status", "running");
  bridge.flush();
  expect(frames).toHaveLength(1);
  expect(replica.chat.getMap("root").get("message")).toBe("live");
  expect(replica.session.getMap("root").get("status")).toBe("running");
  connection.unsubscribe();
  const vector = replica.stateVector()!;
  chat.getMap("root").set("message", "offline");
  const resume = decodeResume({ ...vector, chat: Buffer.from(vector.chat).toString("base64"), session: Buffer.from(vector.session).toString("base64") });
  const resumed = bridge.subscribe(() => {}, { resume });
  expect(resumed.snapshot.mode).toBe("delta");
  replica.applySnapshot(snapshot(resumed.snapshot));
  expect(replica.chat.getMap("root").get("message")).toBe("offline");
  resumed.unsubscribe(); await bridge.close(); replica.destroy(); chat.destroy(); session.destroy();
});

test("real HTTP SSE reconnect sends only missing Yjs content and diagnostics are opt in", async () => {
  const chat = new Y.Doc(); const session = new Y.Doc();
  chat.getText("text").insert(0, "history".repeat(1000));
  const bridge = new SessionDocStream(chat, session);
  const events = new SessionEventLog(); events.append({ jsonrpc: "2.0", method: "private-diagnostic" });
  const app = new Hono();
  app.post("/sync", async (c) => {
    const body = await c.req.json();
    return streamSSE(c, (stream) => streamSessionDocuments(stream, c.req.raw.signal, { docs: bridge, events },
      { after: 0, resume: decodeResume(body.resume), diagnostics: body.diagnostics === true }));
  });
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, fetch: app.fetch });
  const replica = new SessionDocReplica();
  const controllers: AbortController[] = [];
  async function connect(resume?: unknown) {
    const controller = new AbortController(); controllers.push(controller);
    const response = await fetch(`http://127.0.0.1:${server.port}/sync`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ resume }), signal: controller.signal,
    });
    const reader = response.body!.getReader(); const decoder = new TextDecoder(); let buffer = "";
    return { controller, reader, next: async () => {
      while (!buffer.includes("\n\n")) {
        const next = await reader.read();
        if (next.done) throw new Error("Unexpected SSE end");
        buffer += decoder.decode(next.value, { stream: true });
      }
      const end = buffer.indexOf("\n\n"); const frame = buffer.slice(0, end); buffer = buffer.slice(end + 2);
      const fields = Object.fromEntries(frame.split("\n").map((line) => { const index = line.indexOf(":"); return [line.slice(0, index), line.slice(index + 1).trim()]; }));
      return { event: fields.event, data: JSON.parse(fields.data!) };
    } };
  }
  try {
    const first = await connect();
    const initial = await first.next(); expect(initial.event).toBe("docs:snapshot");
    replica.applySnapshot(snapshot(initial.data));
    chat.getText("text").insert(chat.getText("text").length, "tail"); bridge.flush();
    const live = await first.next(); expect(live.event).toBe("docs:update");
    replica.applyUpdate(update(live.data));
    const vector = replica.stateVector()!;
    first.controller.abort(); await first.reader.cancel().catch(() => {});
    chat.getText("text").insert(chat.getText("text").length, "offline");
    const second = await connect({ ...vector, chat: Buffer.from(vector.chat).toString("base64"), session: Buffer.from(vector.session).toString("base64") });
    const resumed = await second.next();
    expect(resumed.event).toBe("docs:snapshot"); expect(resumed.data.mode).toBe("delta");
    expect(resumed.data.chat.length).toBeLessThan(initial.data.chat.length / 10);
    replica.applySnapshot(snapshot(resumed.data));
    expect(replica.chat.getText("text").toString()).toBe("history".repeat(1000) + "tailoffline");
    second.controller.abort(); await second.reader.cancel().catch(() => {});
  } finally {
    for (const controller of controllers) controller.abort();
    await server.stop(true); await bridge.close(); replica.destroy(); chat.destroy(); session.destroy();
  }
});
