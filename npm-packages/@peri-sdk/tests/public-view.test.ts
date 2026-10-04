import { expect, test } from "bun:test";
import * as Y from "yjs";
import { SessionDocs, SessionDocSync } from "../dist/index.js";
import { SessionViewStore, SessionDocReplica } from "../dist/view/index.js";

test("the SDK and browser view package entrypoints share Yjs type identity", async () => {
    const docs = new SessionDocs(); docs.acceptDeliveredUserInput("user", "public entrypoints");
    expect(docs.chat).toBeInstanceOf(Y.Doc);
    const direct = new SessionViewStore(docs.chat, docs.session);
    expect(direct.getSnapshot().entries[0]?.blocks[0]).toMatchObject({ text: "public entrypoints" });
    const sync = new SessionDocSync(docs.chat, docs.session);
    const replica = new SessionDocReplica();
    const connection = sync.subscribe((frame) => replica.applyUpdate(frame)); replica.applySnapshot(connection.snapshot);
    const remote = new SessionViewStore(replica.chat, replica.session);
    docs.accept({ jsonrpc: "2.0", method: "session/update", params: { update: {
        sessionUpdate: "agent_message_chunk", content: { text: "stream" },
    } } });
    sync.flush(); await Promise.resolve();
    expect(remote.getSnapshot().entries[1]?.blocks[0]).toMatchObject({ text: "stream" });
    direct.destroy(); remote.destroy(); connection.unsubscribe(); await sync.close(); replica.destroy(); docs.destroy();
});
