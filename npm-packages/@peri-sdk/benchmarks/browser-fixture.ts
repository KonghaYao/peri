import { Hono } from 'hono';
import { streamSSE } from 'hono/streaming';
import { SessionDocs } from '../src/state/session-docs.ts';
import { SessionDocStream, decodeResume } from '../examples/demo/session-doc-stream.ts';
import { SessionEventLog } from '../examples/demo/session-event-log.ts';
import { streamSessionDocuments } from '../examples/demo/session-sse.ts';
const base = new URL('../', import.meta.url).pathname;
const app = new Hono();
const docs = new SessionDocs();docs.acceptDeliveredUserInput('u','12,000 tool smoke');
const emit=(sessionUpdate:string,fields:any)=>docs.accept({jsonrpc:'2.0',method:'session/update',params:{sessionId:'smoke',update:{sessionUpdate,...fields}}});
for(let i=0;i<12000;i++) {
 if(i%10===0) emit('agent_message_chunk',{messageId:`m${i}`,content:{text:`step ${i}`}});
 emit('tool_call',{toolCallId:`tool${i}`,title:`Read ${i}`,status:'completed',rawInput:{index:i},rawOutput:'payload-'.repeat(1024)});
}
const bridge = new SessionDocStream(docs.chat,docs.session);const events=new SessionEventLog();
let payloadReads=0;
const view = await Bun.build({entrypoints:[`${base}/src/view/index.ts`],target:'browser',format:'esm'});
const sse = await Bun.build({entrypoints:[`${base}/node_modules/@microsoft/fetch-event-source/lib/esm/index.js`],target:'browser',format:'esm'});
if(!view.success || !sse.success) throw new Error('bundle failed');
const html=await Bun.file(`${base}/examples/demo/demo.html`).text();
app.get('/',c=>c.html(html));
app.get('/vendor/peri-session-view.js',async c=>c.body(await view.outputs[0]!.text(),200,{'Content-Type':'text/javascript'}));
app.get('/vendor/fetch-event-source.js',async c=>c.body(await sse.outputs[0]!.text(),200,{'Content-Type':'text/javascript'}));
app.post('/api/session/list',c=>c.json({sessions:[{id:'smoke',title:'Stress smoke',cwd:'/fixture',updatedAt:'2026-10-05'}]}));
app.post('/api/session/create',async c=>{
 const body=await c.req.json();return streamSSE(c,async stream=>{
 await stream.writeSSE({event:'session',data:JSON.stringify({sessionId:'smoke',agentId:'smoke',workspace:'/fixture'})});
 await streamSessionDocuments(stream,c.req.raw.signal,{docs:bridge,events},{after:0,resume:decodeResume(body.resume),diagnostics:false});
 });
});
app.post('/api/session/payload',async c=>{const {id}=await c.req.json();payloadReads++;const payload=docs.readPayload(id);return payload===undefined?c.json({error:'missing'},404):c.body(payload,200,{'Content-Type':'application/json'});});
app.post('/advance',c=>{emit('agent_message_chunk',{content:{text:' live-tail'}});bridge.flush();return c.json({ok:true});});
app.post('/long',c=>{emit('agent_message_chunk',{messageId:'long-output',content:{text:'z'.repeat(128*1024)}});bridge.flush();return c.json({ok:true});});
app.get('/stats',c=>c.json({payloadReads}));
export const server=Bun.serve({hostname:'127.0.0.1',port:0,fetch:app.fetch});
export async function closeFixture() { server.stop(true); await bridge.close(); docs.destroy(); }
if (import.meta.main) console.log(`http://127.0.0.1:${server.port}/?sessionId=smoke`);
