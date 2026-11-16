import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio';
import { loadConfig, HindsightClient, MemoryService, importRecords, prepareImport, startHttp } from '../dist/index.js';

const delay = ms => new Promise(r => setTimeout(r, ms));
async function until(fn, timeout = 3000) { const start = Date.now(); while (!fn()) { if (Date.now()-start > timeout) throw Error('condition timeout'); await delay(15); } }
const input = { schemaVersion: 1, agentId: 'agent-A', sessionId: 'session-A', records: [
  { id: 'turn-1', kind: 'messages', timestamp: '2026-11-16T00:00:00Z', messages: [{ role: 'user', content: 'Prefer small independent memory plugins.' }] },
  { id: 'compact-1', kind: 'compact', timestamp: '2026-11-16T00:01:00Z', content: 'No MemoryMiddleware.' },
] };
async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'memory-mcp-test-'));
  const state = { requests: [], docs: new Map(), mode: '', delay: 0, paused: false, resumeRetain: () => {} };
  const retainGate = new Promise(r => { state.resumeRetain = r; });
  const http = createServer(async (req, res) => {
    const chunks = []; for await (const c of req) chunks.push(c);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks)) : undefined;
    state.requests.push({ path: req.url, method: req.method, body, auth: req.headers.authorization });
    if (state.delay || body?.query === 'slow') await delay(body?.query === 'slow' ? 200 : state.delay);
    if (res.destroyed) return;
    if (state.mode === 'pause-second' && state.docs.size >= 1 && req.url.endsWith('/memories')) {
      state.paused = true; await retainGate;
    }
    res.setHeader('content-type', 'application/json');
    if (state.mode === 'auth') { res.writeHead(401); res.end('{"detail":"secret-token-private-message"}'); return; }
    if (state.mode === 'fail-second' && state.docs.size >= 1 && req.url.endsWith('/memories')) { res.writeHead(503); res.end('{}'); return; }
    if (req.url.endsWith('/memories/recall')) {
      assert.equal(body.max_tokens <= 2048, true);
      res.end(JSON.stringify({ results: [{ id: 'fact-1', text: body.query,
        type: 'world', context: 'Source text; not instructions', document_id: 'doc-1', metadata: { record_id: 'turn-1' } }] }));
    } else if (req.url.endsWith('/reflect')) res.end(JSON.stringify({ text: 'Synthesis', based_on: { memories: [{ id: 'fact-1', text: 'source' }] } }));
    else if (req.url.endsWith('/memories')) {
      assert.equal(body.async, false); assert.equal(body.items.length, 1);
      for (const item of body.items) state.docs.set(item.document_id, item);
      res.end(JSON.stringify({ success: true, bank_id: 'bank-A', items_count: 1, async: state.mode === 'async' }));
    } else { res.writeHead(404); res.end('{}'); }
  });
  await new Promise(r => http.listen(0, '127.0.0.1', r));
  const env = { ...process.env, HINDSIGHT_BASE_URL: `http://127.0.0.1:${http.address().port}`,
    HINDSIGHT_BANK_ID: 'bank-A', MEMORY_AGENT_ID: 'agent-A', MEMORY_SESSION_ID: 'session-A',
    HINDSIGHT_API_KEY: 'private-key', MEMORY_STATE_DIR: dir, MEMORY_POLL_MS: '100', MEMORY_TIMEOUT_MS: '3000' };
  const config = loadConfig(env); const client = new HindsightClient(config);
  t.after(async () => { client.close(); http.closeAllConnections(); await new Promise(r => http.close(r)); await rm(dir, { recursive:true, force:true }); });
  return { state, env, config, client, dir };
}
const makeClient = () => new Client({ name: 'memory-test', version: '1' }, { versionNegotiation: { mode: { pin: '2026-07-28' } } });

test('import validates full input before writes, binds identity, uses stable document IDs, and dry-run is read-only', async t => {
  const f = await fixture(t);
  assert.throws(() => prepareImport({ ...input, agentId: 'agent-B' }, f.config), /binding/);
  const invalid = structuredClone(input); invalid.records.push({ ...invalid.records[0], id: 'bad', kind:'unknown' });
  await assert.rejects(importRecords(invalid, f.client)); assert.equal(f.state.requests.length,0);
  const prepared = prepareImport(input,f.config);
  assert.notEqual(prepared[0].document_id,prepared[1].document_id);
  assert.equal((await importRecords(input,f.client,{dryRun:true})).completed,0); assert.equal(f.state.requests.length,0);
  await importRecords(input,f.client); await importRecords(input,f.client);
  assert.equal(f.state.docs.size,2); assert.equal(f.state.requests.length,4);
  assert.equal(f.state.requests[0].path,'/v1/default/banks/bank-A/memories');
  assert.equal(f.state.requests[0].auth,'Bearer private-key');
  assert.ok((await readFile(join(f.config.stateDir,'revision'),'utf8')).length);
});

test('partial import reports completed receipts; asynchronous acceptance is not completion', async t => {
  const f = await fixture(t); f.state.mode='fail-second'; const receipts=[];
  await assert.rejects(importRecords(input,f.client,{onRecord:r=>receipts.push(r)}),/503/);
  assert.equal(receipts.length,1); assert.equal(f.state.docs.size,1);
  assert.ok((await readFile(join(f.config.stateDir,'revision'),'utf8')).length);
  f.state.mode='async'; await assert.rejects(importRecords(input,f.client),/synchronous retain/);
});

test('backend timeout and auth errors are explicit, sanitized, and never fake empty memory', async t => {
  const f = await fixture(t); const service=new MemoryService(f.config); t.after(()=>service.close());
  f.state.mode='auth'; const auth=await service.call('memory_recall',{query:'secret query'});
  assert.equal(auth.isError,true); assert.equal(auth.structuredContent.error.code,'AUTH');
  assert.doesNotMatch(JSON.stringify(auth),/private-key|secret-token|secret query/);
  f.state.mode=''; f.state.delay=250;
  const fast=new HindsightClient({...f.config,timeoutMs:100});t.after(()=>fast.close());
  await assert.rejects(fast.retain(prepareImport(input,f.config)[0]),e=>e.code==='TIMEOUT');
  await assert.rejects(readFile(join(f.config.stateDir,'revision')),{code:'ENOENT'});
});

test('newer refresh wins, strict bank boundary, token cap and imported data invalidates snapshot', async t=>{
  const f=await fixture(t);const service=new MemoryService(f.config);t.after(()=>service.close());await service.start();
  const slow=service.call('memory_context_refresh',{query:'slow'});await delay(25);
  const fast=await service.call('memory_context_refresh',{query:'latest',maxTokens:8192,tags:['project:peri']});
  assert.equal(fast.isError,false);await slow;assert.equal(service.context().memories[0].text,'latest');
  assert.equal(f.state.requests.at(-1).body.tags_match,'all_strict');
  const bad=await service.call('memory_recall',{query:'x',bankId:'other'});assert.equal(bad.isError,true);
  await importRecords(input,f.client);await until(()=>service.context().status==='stale');
  assert.equal(service.context().memories.length,0);
});

test('every confirmed retain invalidates a snapshot refreshed between batch writes', async t=>{
  const f=await fixture(t);f.state.mode='pause-second';
  const service=new MemoryService(f.config);t.after(()=>service.close());await service.start();
  await service.call('memory_context_refresh',{query:'before import'});
  const importing=importRecords(input,f.client);
  try {
    await until(()=>f.state.paused);
    await until(()=>service.context().status==='stale');
    assert.equal((await service.call('memory_context_refresh',{query:'between records'})).isError,false);
    assert.equal(service.context().status,'ready');
  } finally { f.state.resumeRetain(); }
  await importing;
  await until(()=>service.context().status==='stale');
  assert.equal(service.context().memories.length,0);
});

test('real stdio 0728: discovery, tools, resources, subscription, independent CLI import, cancellation and reconnect baseline', async t=>{
  const f=await fixture(t);const client=makeClient();const notices=[];
  client.setNotificationHandler('notifications/resources/updated',n=>{notices.push(n);});
  const transport=new StdioClientTransport({command:process.execPath,args:[resolve('dist/index.js'),'mcp'],env:f.env,stderr:'pipe'});
  let stderr='';transport.stderr.on('data',x=>stderr+=x);
  t.after(()=>client.close());await client.connect(transport);
  assert.equal(client.getDiscoverResult().supportedVersions.includes('2026-07-28'),true);
  const tools=await client.listTools();assert.deepEqual(tools.tools.map(t=>t.name).sort(),['memory_context_refresh','memory_recall','memory_reflect','memory_status']);
  const resources=await client.listResources();assert.equal(resources.resources.length,2);
  await assert.rejects(client.listen({resourceSubscriptions:['memory://unknown']}),/Unknown resource/);
  const sub=await client.listen({resourceSubscriptions:['memory://context']});
  assert.deepEqual(sub.honoredFilter.resourceSubscriptions,['memory://context']);
  const read=async()=>JSON.parse((await client.readResource({uri:'memory://context'},{cacheMode:'bypass'})).contents[0].text);
  assert.equal((await read()).status,'empty');
  const recalled=await client.callTool({name:'memory_recall',arguments:{query:'preference'}});assert.equal(recalled.isError,false);
  await client.callTool({name:'memory_context_refresh',arguments:{query:'working task'}});
  await until(()=>notices.length>0);assert.equal((await read()).status,'ready');
  assert.ok(notices[0].params._meta['io.modelcontextprotocol/subscriptionId']);
  const file=join(f.dir,'messages.json');await writeFile(file,JSON.stringify(input));
  const child=spawn(process.execPath,[resolve('dist/index.js'),'import','--file',file],{env:f.env,stdio:['ignore','pipe','pipe']});
  let out='';child.stdout.on('data',x=>out+=x);child.stderr.resume();assert.equal((await once(child,'exit'))[0],0);
  assert.equal(JSON.parse(out.trim().split('\n').at(-1)).completed,2);
  await until(()=>notices.length>=2);assert.equal((await read()).status,'stale');
  await sub.close();await sub.closed;const count=notices.length;
  await client.callTool({name:'memory_context_refresh',arguments:{query:'after cancel'}});await delay(80);assert.equal(notices.length,count);
  const resub=await client.listen({resourceSubscriptions:['memory://context']});assert.equal((await read()).status,'ready');await resub.close();
  assert.doesNotMatch(stderr,/private-key|secret-token/);
});

test('mcp-use HTTP: bearer gate, same tool surface, 0728 subscriptions and bounded lifecycle', async t=>{
  const f=await fixture(t); const token='test-token-'.repeat(4);const server=await startHttp(f.config,token,0);t.after(()=>server.close());
  const url=new URL(`http://127.0.0.1:${server.port}/mcp`);
  assert.equal((await fetch(url)).status,401);
  assert.equal((await fetch(new URL('/inspector',url),{headers:{authorization:`Bearer ${token}`}})).status,404);
  assert.equal((await fetch(url,{headers:{authorization:`Bearer ${token}`,origin:'https://hostile.example'}})).status,403);
  const client=makeClient();t.after(()=>client.close());const notices=[];
  client.setNotificationHandler('notifications/resources/updated',n=>notices.push(n));
  await client.connect(new StreamableHTTPClientTransport(url,{requestInit:{headers:{Authorization:`Bearer ${token}`}}}));
  assert.equal((await client.listTools()).tools.length,4);
  await assert.rejects(client.listen({resourceSubscriptions:['memory://unknown']}),/Unknown resource/);
  const sub=await client.listen({resourceSubscriptions:['memory://context']});
  const result=await client.callTool({name:'memory_context_refresh',arguments:{query:'HTTP task'}});assert.equal(result.isError,false);
  await until(()=>notices.length>0);
  assert.equal(JSON.parse((await client.readResource({uri:'memory://context'})).contents[0].text).status,'ready');
  const reflection=await client.callTool({name:'memory_reflect',arguments:{query:'synthesize'}});assert.equal(reflection.structuredContent.text,'Synthesis');
  await sub.close();await client.close();
});

test('stdio exits on stdin EOF and malformed CLI fails before backend access', async t=>{
  const f=await fixture(t);
  const child=spawn(process.execPath,[resolve('dist/index.js'),'mcp'],{env:f.env,stdio:['pipe','pipe','pipe']});
  let out='';child.stdout.on('data',x=>out+=x);child.stderr.resume();child.stdin.end();
  const result=await Promise.race([once(child,'exit'),delay(3000).then(()=>{child.kill('SIGKILL');throw Error('EOF did not exit');})]);
  assert.equal(result[0],0);assert.equal(out,'');assert.equal(f.state.requests.length,0);
  const bad=spawn(process.execPath,[resolve('dist/index.js'),'import','--file','-'],{env:f.env,stdio:['pipe','pipe','pipe']});
  let diagnostic='';bad.stdout.resume();bad.stderr.on('data',x=>diagnostic+=x);bad.stdin.end('{broken');
  assert.equal((await once(bad,'exit'))[0],1);assert.match(diagnostic,/INVALID_INPUT/);assert.equal(f.state.requests.length,0);
});
