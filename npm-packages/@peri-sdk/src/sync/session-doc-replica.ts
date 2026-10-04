import * as Y from "yjs";
import type { DocSnapshot, DocStateVector, DocUpdate } from "./session-doc-sync";

/** Owns a read-only document pair and rejects gaps until a snapshot/delta repairs them. */
export class SessionDocReplica {
    chat = new Y.Doc();
    session = new Y.Doc();
    private generation: string | undefined;
    private sequence = -1;
    private closed = false;

    stateVector(): DocStateVector | undefined {
        if (!this.generation || this.closed) return undefined;
        return { protocol: 2, generation: this.generation,
            chat: Y.encodeStateVector(this.chat), session: Y.encodeStateVector(this.session) };
    }

    applySnapshot(snapshot: DocSnapshot): "replaced" | "updated" {
        this.validate(snapshot);
        if (snapshot.mode !== "snapshot" && snapshot.mode !== "delta") throw new Error("Invalid Yjs snapshot mode");
        if (!(snapshot.chat instanceof Uint8Array) || !(snapshot.session instanceof Uint8Array)) throw new Error("Invalid Yjs snapshot bytes");
        if (snapshot.mode === "delta") {
            if (snapshot.generation !== this.generation || snapshot.sequence < this.sequence) throw new Error("Yjs resume generation or sequence mismatch");
            Y.applyUpdateV2(this.chat, snapshot.chat);
            Y.applyUpdateV2(this.session, snapshot.session);
        } else {
            const chat = new Y.Doc();
            const session = new Y.Doc();
            try { Y.applyUpdateV2(chat, snapshot.chat); Y.applyUpdateV2(session, snapshot.session); }
            catch (error) { chat.destroy(); session.destroy(); throw error; }
            this.chat.destroy();
            this.session.destroy();
            this.chat = chat;
            this.session = session;
        }
        this.generation = snapshot.generation;
        this.sequence = snapshot.sequence;
        return snapshot.mode === "snapshot" ? "replaced" : "updated";
    }

    applyUpdate(update: DocUpdate): void {
        this.validate(update);
        if (update.generation !== this.generation) throw new Error("Yjs update generation mismatch");
        if (update.sequence <= this.sequence) return;
        if (update.sequence !== this.sequence + 1) throw new Error("Yjs update sequence gap; resume required");
        if ((!update.chat && !update.session) || (update.chat && !(update.chat instanceof Uint8Array)) ||
            (update.session && !(update.session instanceof Uint8Array))) throw new Error("Invalid Yjs update bytes");
        if (update.chat) Y.applyUpdateV2(this.chat, update.chat);
        if (update.session) Y.applyUpdateV2(this.session, update.session);
        this.sequence = update.sequence;
    }

    destroy(): void {
        if (this.closed) return;
        this.closed = true;
        this.chat.destroy();
        this.session.destroy();
    }

    private validate(frame: DocSnapshot | DocUpdate): void {
        if (this.closed) throw new Error("Yjs replica is closed");
        if (frame.protocol !== 2 || typeof frame.generation !== "string" || !frame.generation ||
            !Number.isSafeInteger(frame.sequence) || frame.sequence < 0) throw new Error("Invalid Yjs sync frame");
    }
}
