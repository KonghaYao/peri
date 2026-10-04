import * as Y from "yjs";
import { SessionDocSync, type DocStateVector, type DocUpdate as BinaryUpdate } from "../../src/sync/index";

export type DocSnapshot = {
  protocol: 2; generation: string; sequence: number; mode: "snapshot" | "delta";
  chat: string; session: string;
};
export type DocUpdate = { protocol: 2; generation: string; sequence: number; chat?: string; session?: string };
export type DocResume = { protocol: 2; generation: string; chat: string; session: string };
const base64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64");

export function decodeResume(value: unknown): DocStateVector | undefined {
  if (value === undefined || value === null) return undefined;
  const resume = value as DocResume;
  if (resume.protocol !== 2 || typeof resume.generation !== "string" || !resume.generation ||
    typeof resume.chat !== "string" || typeof resume.session !== "string" ||
    resume.chat.length > 90_000 || resume.session.length > 90_000) throw new Error("Invalid document resume state");
  return { protocol: 2, generation: resume.generation,
    chat: Uint8Array.from(Buffer.from(resume.chat, "base64")), session: Uint8Array.from(Buffer.from(resume.session, "base64")) };
}

/** SSE text adapter over the SDK's binary, bounded Yjs sync protocol. */
export class SessionDocStream {
  private readonly sync: SessionDocSync;
  private readonly encoded = new WeakMap<BinaryUpdate, DocUpdate>();
  constructor(chat: Y.Doc, session: Y.Doc) { this.sync = new SessionDocSync(chat, session); }

  subscribe(listener: (event: DocUpdate) => void | Promise<void>, options: {
    resume?: DocStateVector; onError?: (error: Error) => void;
  } = {}): { snapshot: DocSnapshot; unsubscribe: () => void } {
    const connection = this.sync.subscribe((frame) => {
      let event = this.encoded.get(frame);
      if (!event) {
        event = { protocol: 2, generation: frame.generation, sequence: frame.sequence,
          ...(frame.chat ? { chat: base64(frame.chat) } : {}),
          ...(frame.session ? { session: base64(frame.session) } : {}) };
        this.encoded.set(frame, event);
      }
      return listener(event);
    }, options);
    return { snapshot: { ...connection.snapshot,
      chat: base64(connection.snapshot.chat), session: base64(connection.snapshot.session) },
      unsubscribe: connection.unsubscribe };
  }

  flush(): void { this.sync.flush(); }
  close(): Promise<void> { return this.sync.close(); }
}
