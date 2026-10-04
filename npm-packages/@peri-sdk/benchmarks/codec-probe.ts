import * as Y from "yjs";
import { SessionDocs } from "../src/state/session-docs.ts";
import { readSessionView } from "../src/view/session-view.ts";
import { createHash } from "node:crypto";
const variant = process.argv[2];
const docs = new SessionDocs();
const peer = new Y.Doc();
const v2 = variant === "v2";
const converted = variant === "convert";
const batched = variant !== "direct";
let pending: Uint8Array[] = [], bytes=0, size=0, frames=0;
const flush = () => {
 if (!pending.length) return;
 const result = pending.length === 1 ? pending[0]! : v2 ? Y.mergeUpdatesV2(pending) : Y.mergeUpdates(pending);
 const packet = converted ? Y.convertUpdateFormatV1ToV2(result) : result;
 size += packet.length; frames++;
 if (v2 || converted) Y.applyUpdateV2(peer,packet); else Y.applyUpdate(peer,packet);
 pending=[];bytes=0;
};
if (v2) Y.applyUpdateV2(peer,Y.encodeStateAsUpdateV2(docs.chat));
else Y.applyUpdate(peer,Y.encodeStateAsUpdate(docs.chat));
docs.chat.on(v2?'updateV2':'update', (delta:Uint8Array) => {
 if (bytes && bytes+delta.length>65536) flush();
 pending.push(delta); bytes+=delta.length;
 if (!batched || bytes>=65536 || pending.length>=256) flush();
});
docs.acceptDeliveredUserInput('u','go');
const t = performance.now();
for (let i=0;i<16384;i++) {
 const text = `${i}:`.padEnd(1024, 'x');
 docs.accept({jsonrpc:'2.0',method:'session/update',params:{update:{sessionUpdate:'agent_message_chunk',content:{text}}}});
}
docs.completeTurn();flush();
const ms = performance.now()-t;
const view = readSessionView(peer,docs.session);
const actual = view.entries.flatMap(e=>e.blocks).flatMap(b=>b.type==='text' && b.text!=='go' ? [b.text] : []).join('');
const expected = Array.from({length:16384},(_,i)=>`${i}:`.padEnd(1024,'x')).join('');
const sha = (text:string)=>createHash('sha256').update(text).digest('hex');
if (sha(actual)!==sha(expected) || actual.length!==16*1024*1024 || view.activeTurnStatus!=='completed') throw new Error('codec probe content mismatch');
console.log(JSON.stringify({variant,ms,size,frames,exact:true,textBytes:actual.length,sha256:sha(actual)}));
docs.destroy();peer.destroy();
