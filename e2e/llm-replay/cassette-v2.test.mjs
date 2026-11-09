import test from 'node:test';
import assert from 'node:assert/strict';
import { cp, mkdtemp, readFile, rename, rm, unlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createCassetteWriter, loadCassette } from './cassette.mjs';
import { canonicalBody, sha256 } from './matcher.mjs';
import { createRequestBindingAdapter } from './request-bindings.mjs';
import { createWorldBuilder } from './world-store.mjs';

async function temporary(t, prefix = 'peri-cassette-v2-') {
  const directory = await mkdtemp(path.join(os.tmpdir(), prefix));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

const identity = (index = 1) => ({ owner: 'root', execution: 'turn-1', call: `reason:${index}`, attempt: 1, purpose: 'reason' });

function anthropicBody(root = '/record/root') {
  return Buffer.from(JSON.stringify({
    model: 'fixture-model', max_tokens: 32000, stream: true,
    system: [
      { type: 'text', text: `strict instruction\n<env>\nWorking directory: ${root}/work\nToday's date: 2026-11-07\nSession ID: session-recorded\n</env>` },
      { type: 'text', text: 'skills catalog' },
    ],
    messages: [{ role: 'user', content: 'same question' }],
    tools: [{ name: 'Write', input_schema: { type: 'object' } }],
  }));
}

function bindWorld(adapter) {
  for (const [alias, kind, live] of [
    ['root', 'path', '/record/root'], ['work', 'path', '/record/root/work'],
    ['today', 'date', '2026-11-07'], ['session', 'session', 'session-recorded'],
  ]) adapter.declareAndBind({ kind, alias, live, anchor: alias });
}

async function recordAll(t, { bodies = [anthropicBody()], world, suffix = '' } = {}) {
  const directory = await temporary(t);
  const cassetteDir = path.join(directory, `case${suffix}`, 'cassette');
  const writer = await createCassetteWriter(cassetteDir, { world, provenance: { source: 'test' } });
  const rulesByEntry = [];
  for (const [index, body] of bodies.entries()) {
    const call = identity(index + 1);
    const adapter = createRequestBindingAdapter({ mode: 'record', runId: `record-${index}` });
    bindWorld(adapter);
    adapter.onRequest({ context: call, request: { body } });
    const rules = adapter.requestRules(call);
    const canonical = canonicalBody(body, { bindings: adapter.registry, owner: 'world', rules });
    const capture = await writer.begin(call, {
      method: 'POST', target: 'https://fixture.test/v1/messages',
      headers: { 'content-type': 'application/json' }, body, canonicalBody: canonical,
    }, rules, { bindings: adapter.registry, owner: 'world' });
    capture.setHeaders(200, { 'content-type': 'text/event-stream' }, 0);
    const response = Buffer.from(`data: answer-${index + 1}\n\n`);
    await capture.append(response, index);
    capture.setSubmittedBytes(response.length);
    await capture.finish('complete', index + 1);
    rulesByEntry.push({ rules, canonical, body });
  }
  await writer.seal();
  return { directory, cassetteDir, writer, rulesByEntry };
}

test('v2 writer/loader 保留内存形状：canonicalBody 由 stub 重建，rules 展开为 segments', async (t) => {
  const fixture = await recordAll(t);
  const cassette = await loadCassette(fixture.cassetteDir);
  assert.equal(cassette.manifest.formatVersion, 2);
  assert.equal(cassette.report.formatVersion, 2);
  assert.deepEqual(cassette.report.legacy, []);
  const [entry] = cassette.entries.values();
  assert.equal(entry.request.file, undefined);
  assert.match(entry.request.stub, /^requests\/[0-9a-f]{64}\.stub\.json$/u);
  assert.equal(entry.request.canonicalBody, fixture.rulesByEntry[0].canonical);
  assert.deepEqual(entry.requestRules[0].segments, fixture.rulesByEntry[0].rules[0].segments);
  assert.equal(entry.requestRules[0].pattern, undefined);
  const seal = JSON.parse(await readFile(path.join(fixture.cassetteDir, 'seal.json'), 'utf8'));
  assert.equal(seal.formatVersion, 2);
  assert.ok(seal.files[entry.request.stub]);
  assert.equal(seal.files['requests/' + sha256(JSON.stringify(['root', 'turn-1', 'reason:1', 1, 'reason'])) + '.bin'], undefined);
  const stub = JSON.parse(await readFile(path.join(fixture.cassetteDir, entry.request.stub), 'utf8'));
  assert.equal(stub.v, 2);
  assert.equal(stub.rawSha256, sha256(fixture.rulesByEntry[0].body));
  assert.equal(stub.canonicalSha256, sha256(fixture.rulesByEntry[0].canonical));
  assert.equal(stub.parts.some((part) => typeof part === 'string'), true);
});

test('v2 录制时 live 与 recorded 不同，仍按 live 定位并封存 recorded canonical', async (t) => {
  const directory = await temporary(t);
  const cassetteDir = path.join(directory, 'case', 'cassette');
  const body = Buffer.from(JSON.stringify({ system: [{ type: 'text', text: 'cwd /live/root/work' }], messages: [] }));
  const adapter = createRequestBindingAdapter({ mode: 'record', runId: 'record', declarations: [
    { owner: 'world', kind: 'path', alias: 'work', recorded: '/recorded/root/work', anchor: 'work' },
  ] });
  adapter.declareAndBind({ kind: 'path', alias: 'work', live: '/live/root/work', anchor: 'work' });
  adapter.onRequest({ context: identity(), request: { body } });
  const rules = adapter.requestRules(identity());
  const canonical = canonicalBody(body, { bindings: adapter.registry, owner: 'world', rules });
  assert.match(canonical, /\/recorded\/root\/work/u);
  const writer = await createCassetteWriter(cassetteDir, { provenance: { source: 'test' } });
  const capture = await writer.begin(identity(), {
    method: 'POST', target: 'https://fixture.test/v1/messages', headers: { 'content-type': 'application/json' }, body, canonicalBody: canonical,
  }, rules, { bindings: adapter.registry, owner: 'world' });
  capture.setHeaders(200, {}, 0);
  capture.setSubmittedBytes(0);
  await capture.finish('complete', 1);
  await writer.seal();
  const cassette = await loadCassette(cassetteDir);
  const [entry] = cassette.entries.values();
  assert.equal(entry.request.canonicalBody, canonical);
  assert.deepEqual(entry.requestRules[0].segments, rules[0].segments);
});

test('v2 cassette 引用共享 world，默认目录位于 fixture 根的 world/', async (t) => {
  const builder = createWorldBuilder({ generation: 'test' });
  const fixture = await recordAll(t, { world: builder });
  await builder.seal(path.join(fixture.directory, 'world'));
  const cassette = await loadCassette(fixture.cassetteDir);
  const [entry] = cassette.entries.values();
  assert.equal(entry.request.canonicalBody, fixture.rulesByEntry[0].canonical);
  const stub = JSON.parse(await readFile(path.join(fixture.cassetteDir, entry.request.stub), 'utf8'));
  assert.ok(stub.parts.some((part) => part.leaf && part.as === 'json-string'));
  assert.ok(stub.parts.some((part) => part.leaf && part.as === 'raw'));
});

test('v2 seal 覆盖 stub 与 manifest：篡改、删除、互换、响应漂移都失败', async (t) => {
  const fixture = await recordAll(t, { bodies: [anthropicBody(), anthropicBody('/record/other')] });
  const cassette = await loadCassette(fixture.cassetteDir);
  const entries = [...cassette.entries.values()];
  assert.equal(entries.length, 2);
  const [first, second] = entries.map((entry) => entry.request.stub);

  await t.test('stub tamper', async (subtest) => {
    const directory = await temporary(subtest);
    const copied = path.join(directory, 'cassette');
    await cp(fixture.cassetteDir, copied, { recursive: true });
    const stub = JSON.parse(await readFile(path.join(copied, first), 'utf8'));
    stub.rawSha256 = '0'.repeat(64);
    await writeFile(path.join(copied, first), JSON.stringify(stub));
    await assert.rejects(loadCassette(copied), (error) => error.code === 'corrupt_cassette' && error.details.entry === sha256(JSON.stringify(['root', 'turn-1', 'reason:1', 1, 'reason'])));
  });

  await t.test('stub delete', async (subtest) => {
    const directory = await temporary(subtest);
    const copied = path.join(directory, 'cassette');
    await cp(fixture.cassetteDir, copied, { recursive: true });
    await unlink(path.join(copied, first));
    await assert.rejects(loadCassette(copied), { code: 'corrupt_cassette' });
  });

  await t.test('stub swap', async (subtest) => {
    const directory = await temporary(subtest);
    const copied = path.join(directory, 'cassette');
    await cp(fixture.cassetteDir, copied, { recursive: true });
    await rename(path.join(copied, first), path.join(copied, `${first}.tmp`));
    await rename(path.join(copied, second), path.join(copied, first));
    await rename(path.join(copied, `${first}.tmp`), path.join(copied, second));
    await assert.rejects(loadCassette(copied), { code: 'corrupt_cassette' });
  });

  await t.test('manifest tamper', async (subtest) => {
    const directory = await temporary(subtest);
    const copied = path.join(directory, 'cassette');
    await cp(fixture.cassetteDir, copied, { recursive: true });
    const manifest = await readFile(path.join(copied, 'manifest.json'));
    await writeFile(path.join(copied, 'manifest.json'), Buffer.concat([manifest, Buffer.from('\n')]));
    await assert.rejects(loadCassette(copied), { code: 'corrupt_cassette' });
  });

  await t.test('response drift', async (subtest) => {
    const directory = await temporary(subtest);
    const copied = path.join(directory, 'cassette');
    await cp(fixture.cassetteDir, copied, { recursive: true });
    const response = await readFile(path.join(copied, cassette.entries.get([...cassette.entries.keys()][0]).response.file));
    await writeFile(path.join(copied, cassette.entries.get([...cassette.entries.keys()][0]).response.file), Buffer.concat([response, Buffer.from('x')]));
    await assert.rejects(loadCassette(copied), { code: 'corrupt_cassette' });
  });
});

test('v2 stub canonicalSha 被改写且 seal 自洽时仍由重建结果拒绝', async (t) => {
  const fixture = await recordAll(t);
  const cassette = await loadCassette(fixture.cassetteDir);
  const [entry] = cassette.entries.values();
  const stubPath = path.join(fixture.cassetteDir, entry.request.stub);
  const stub = JSON.parse(await readFile(stubPath, 'utf8'));
  stub.canonicalSha256 = '0'.repeat(64);
  const bytes = Buffer.from(`${JSON.stringify(stub, null, 2)}\n`);
  await writeFile(stubPath, bytes);
  const sealPath = path.join(fixture.cassetteDir, 'seal.json');
  const seal = JSON.parse(await readFile(sealPath, 'utf8'));
  seal.files[entry.request.stub] = sha256(bytes);
  await writeFile(sealPath, `${JSON.stringify(seal)}\n`);
  await assert.rejects(loadCassette(fixture.cassetteDir), (error) => error.code === 'corrupt_cassette' && /canonical/u.test(error.message));
});

test('unsafe number 走 legacy.bin，load 时报告并保持 canonical body', async (t) => {
  const body = Buffer.from('{"model":"fixture","n":9007199254740993}');
  const fixture = await recordAll(t, { bodies: [body] });
  const cassette = await loadCassette(fixture.cassetteDir);
  assert.equal(cassette.report.legacy.length, 1);
  assert.equal(cassette.report.legacy[0].reason, 'unsafe_number');
  assert.equal(cassette.entries.size, 1);
  const [entry] = cassette.entries.values();
  assert.equal(entry.request.canonicalBody, canonicalBody(body));
  const stub = JSON.parse(await readFile(path.join(fixture.cassetteDir, entry.request.stub), 'utf8'));
  assert.equal(stub.fallback, 'unsafe_number');
  assert.equal(stub.parts.length, 1);
  assert.match(stub.parts[0].raw, /^requests\/[0-9a-f]{64}\.legacy\.bin$/u);
  const seal = JSON.parse(await readFile(path.join(fixture.cassetteDir, 'seal.json'), 'utf8'));
  assert.ok(seal.files[stub.parts[0].raw]);
});

test('引用 world 的 v2 cassette 在共享 world 缺失时拒绝加载', async (t) => {
  const builder = createWorldBuilder();
  const fixture = await recordAll(t, { world: builder });
  await assert.rejects(loadCassette(fixture.cassetteDir), { code: 'missing_world' });
});

test('v1 fixture 保持可读，并保留 segments 规则形状', async () => {
  const directory = fileURLToPath(new URL('../fixtures/replay-v2/S06/cassette', import.meta.url));
  const cassette = await loadCassette(directory);
  assert.equal(cassette.manifest.formatVersion, 1);
  assert.equal(cassette.report.formatVersion, 1);
  assert.equal(cassette.entries.size, 2);
  for (const entry of cassette.entries.values()) {
    assert.equal(typeof entry.request.canonicalBody, 'string');
    assert.match(entry.request.file, /^requests\/[0-9a-f]{64}\.bin$/u);
    for (const rule of entry.requestRules) {
      assert.equal(rule.mode, 'typed-template');
      assert.ok(Array.isArray(rule.segments));
    }
  }
});
