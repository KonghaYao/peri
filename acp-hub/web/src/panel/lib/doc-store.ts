import * as Y from 'yjs';
import { base64ToBytes } from './protocol';

/** Owns Y.Doc identity, v1 update application and render coalescing only. */
export class DocStore {
  private docs = new Map<string, Y.Doc>();
  private rafPending = new Set<string>();
  private generation = 0;
  onUpdate: ((docId: string) => void) | null = null;

  clear(): void {
    this.generation += 1;
    this.docs.forEach((doc) => doc.destroy());
    this.docs.clear();
    this.rafPending.clear();
  }

  docFor(docId: string): Y.Doc {
    let doc = this.docs.get(docId);
    if (!doc) {
      doc = new Y.Doc();
      this.docs.set(docId, doc);
    }
    return doc;
  }

  applyUpdateFrame(frame: { doc: string; update: string }): void {
    const doc = this.docFor(frame.doc);
    try {
      Y.applyUpdate(doc, base64ToBytes(frame.update));
    } catch {
      console.warn(`applyUpdateFrame 失败（doc=${frame.doc}, update_length=${frame.update.length}）`);
    }
    this.scheduleRender(frame.doc);
  }

  private scheduleRender(docId: string): void {
    if (this.rafPending.has(docId)) return;
    this.rafPending.add(docId);
    const generation = this.generation;
    requestAnimationFrame(() => {
      // A logout/reconnect may clear the store and schedule the same doc id for
      // a new connection before this callback runs. The old callback must not
      // delete or render the new generation's pending work.
      if (generation !== this.generation) return;
      this.rafPending.delete(docId);
      this.onUpdate?.(docId);
    });
  }
}
