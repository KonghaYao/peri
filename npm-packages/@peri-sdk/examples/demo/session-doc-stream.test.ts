import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocStream } from "./session-doc-stream";

function decode(data: string): Uint8Array {
  return Uint8Array.from(Buffer.from(data, "base64"));
}

test("new connection starts from both current docs and follows ordered updates", () => {
  const chat = new Y.Doc();
  const session = new Y.Doc();
  chat.getMap("root").set("message", "history");
  session.getMap("root").set("status", "ready");
  const bridge = new SessionDocStream(chat, session);
  const updates: Array<{ generation: string; sequence: number; doc: string; update: string }> = [];
  const { snapshot, unsubscribe } = bridge.subscribe((entry) => updates.push(entry));

  const browserChat = new Y.Doc();
  const browserSession = new Y.Doc();
  Y.applyUpdate(browserChat, decode(snapshot.chat));
  Y.applyUpdate(browserSession, decode(snapshot.session));
  expect(browserChat.getMap("root").get("message")).toBe("history");
  expect(browserSession.getMap("root").get("status")).toBe("ready");

  chat.getMap("root").set("message", "live");
  session.getMap("root").set("status", "running");
  expect(updates.map(({ sequence, doc }) => ({ sequence, doc }))).toEqual([
    { sequence: snapshot.sequence + 1, doc: "chat" },
    { sequence: snapshot.sequence + 2, doc: "session" },
  ]);
  for (const entry of updates) {
    expect(entry.generation).toBe(snapshot.generation);
    Y.applyUpdate(entry.doc === "chat" ? browserChat : browserSession, decode(entry.update));
  }
  expect(browserChat.getMap("root").get("message")).toBe("live");
  expect(browserSession.getMap("root").get("status")).toBe("running");

  unsubscribe();
  chat.getMap("root").set("message", "later");
  expect(updates).toHaveLength(2);
  const resumed = bridge.subscribe(() => {});
  const resumedChat = new Y.Doc();
  Y.applyUpdate(resumedChat, decode(resumed.snapshot.chat));
  expect(resumed.snapshot.sequence).toBe(snapshot.sequence + 3);
  expect(resumed.snapshot.generation).toBe(snapshot.generation);
  expect(resumedChat.getMap("root").get("message")).toBe("later");
  resumed.unsubscribe();
  bridge.close();
  browserChat.destroy();
  browserSession.destroy();
  resumedChat.destroy();
  chat.destroy();
  session.destroy();
});
