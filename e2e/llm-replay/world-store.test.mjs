import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, cp, readFile, rename, rm, unlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  canonicalRecordedBody, collectWorldRefs, compactTypedTemplateRules, createWorldBuilder, decodeRequestStub,
  encodeRequestStub, expandTypedTemplateRules, loadWorld, requestCoreSha256, worldLeafFile,
} from './world-store.mjs';
import { createRequestBindingAdapter } from './request-bindings.mjs';
import { canonicalBody, canonicalJson, sha256 } from './matcher.mjs';

async function temporary(t, prefix = 'peri-world-') {
  const directory = await mkdtemp(path.join(os.tmpdir(), prefix));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

async function sealedWorld(t, build = () => {}) {
  const directory = await temporary(t);
  const root = path.join(directory, 'world');
  const builder = createWorldBuilder({ generation: 'test' });
  build(builder);
  await builder.seal(root);
  return { directory, root, builder };
}

async function rewriteWorldLeaf(source, target, leafId, content) {
  await cp(source, target, { recursive: true });
  const manifest = JSON.parse(await readFile(path.join(target, 'manifest.json'), 'utf8'));
  await writeFile(path.join(target, manifest.leaves[leafId].file), content);
  const seal = JSON.parse(await readFile(path.join(target, 'seal.json'), 'utf8'));
  seal.leaves[leafId] = sha256(Buffer.from(content));
  await writeFile(path.join(target, 'seal.json'), `${JSON.stringify(seal)}\n`);
  return loadWorld(target);
}

const identity = { owner: 'root', execution: 'turn-1', call: 'reason:1', attempt: 1, purpose: 'reason' };

function anthropicBody(root = '/record/root', today = '2026-11-07') {
  return Buffer.from(JSON.stringify({
    model: 'fixture-model', max_tokens: 32000, stream: true,
    system: [
      { type: 'text', text: `strict instruction\n<env>\nWorking directory: ${root}/work\nToday's date: ${today}\nSession ID: session-recorded\n</env>` },
      { type: 'text', text: 'The skills catalog and tool guidance live here.' },
    ],
    messages: [{ role: 'user', content: 'keep this message exact' }],
    tools: [{ name: 'Write', input_schema: { type: 'object', properties: { file_path: { type: 'string' } } } }],
  }));
}

function responsesBody() {
  return Buffer.from(JSON.stringify({
    model: 'fixture-model', stream: true, store: false,
    input: [
      { type: 'message', role: 'developer', content: [{ type: 'input_text', text: 'developer instructions' }] },
      { type: 'message', role: 'user', content: [{ type: 'input_text', text: 'user message' }] },
    ],
    tools: [{ type: 'function', name: 'Bash', parameters: { type: 'object' } }],
  }));
}

function bindWorld(adapter, values) {
  for (const [alias, [kind, live]] of Object.entries(values)) adapter.declareAndBind({ kind, alias, live, anchor: alias });
}

function recordingAdapter(body) {
  const adapter = createRequestBindingAdapter({ mode: 'record', runId: 'record' });
  bindWorld(adapter, {
    root: ['path', '/record/root'], work: ['path', '/record/root/work'],
    today: ['date', '2026-11-07'], session: ['session', 'session-recorded'],
  });
  adapter.onRequest({ context: identity, request: { body } });
  return { adapter, rules: adapter.requestRules(identity) };
}

test('world 重建保持 manifest 稳定：leaf 内容变化只改 leaf 与 seal', async (t) => {
  const directory = await temporary(t);
  const build = async (skills, name) => {
    const builder = createWorldBuilder();
    builder.addLeaf('sys.skills', skills);
    builder.addLeaf('sys.tail', 'TAIL');
    builder.addRecipe('sys.block1', [{ leaf: 'sys.skills' }, { leaf: 'sys.tail' }]);
    const root = path.join(directory, name);
    await builder.seal(root);
    return root;
  };
  const before = await build('SKILLS-A', 'before');
  const after = await build('SKILLS-B', 'after');
  assert.deepEqual(await readFile(path.join(before, 'manifest.json')), await readFile(path.join(after, 'manifest.json')));
  assert.notDeepEqual(await readFile(path.join(before, 'seal.json')), await readFile(path.join(after, 'seal.json')));
  assert.notDeepEqual(await readFile(path.join(before, 'system/skills.txt')), await readFile(path.join(after, 'system/skills.txt')));
  assert.deepEqual(await readFile(path.join(before, 'system/tail.txt')), await readFile(path.join(after, 'system/tail.txt')));
});

test('world leaf、嵌套 recipe 与内容寻址 id 可确定性渲染', async (t) => {
  const { root } = await sealedWorld(t, (builder) => {
    builder.addLeaf('sys.a', 'A');
    builder.addLeaf('sys.b', 'B');
    builder.addRecipe('sys.nested', ['<', { leaf: 'sys.a' }, '>']);
    builder.addRecipe('sys.outer', [{ recipe: 'sys.nested' }, { leaf: 'sys.b' }]);
  });
  const world = await loadWorld(root);
  assert.equal(world.render({ recipe: 'sys.outer' }).toString(), '<A>B');
  assert.equal(world.renderParts(['x', { recipe: 'sys.nested' }, 'y']).toString(), 'x<A>y');
  assert.equal(worldLeafFile('sys.block1.main'), 'system/block1.main.txt');
  assert.equal(worldLeafFile('tools.Agent.main'), 'tools/Agent.main.json');

  const builder = createWorldBuilder();
  const first = builder.addContentLeaf('system', 'same');
  const second = builder.addContentLeaf('system', 'same');
  assert.equal(first, second);
  assert.match(first, /^sys\.sha256\.[0-9a-f]{64}$/u);
  assert.throws(() => builder.addLeaf(first, 'different'), { code: 'duplicate_world_leaf' });
});

test('world leaf 篡改、删除与互换都会按 leaf id 失败', async (t) => {
  const cases = [
    ['tamper', async (file) => writeFile(file, 'tampered')],
    ['delete', async (file) => unlink(file)],
  ];
  for (const [name, mutate] of cases) {
    await t.test(name, async (subtest) => {
      const { root } = await sealedWorld(subtest, (builder) => {
        builder.addLeaf('sys.a', 'A');
        builder.addLeaf('sys.b', 'B');
      });
      await mutate(path.join(root, 'system/a.txt'));
      await assert.rejects(loadWorld(root), (error) => error.code === 'corrupt_world' && error.details.leaf === 'sys.a');
    });
  }
  await t.test('swap', async (subtest) => {
    const { root } = await sealedWorld(subtest, (builder) => {
      builder.addLeaf('sys.a', 'A');
      builder.addLeaf('sys.b', 'B');
    });
    const left = path.join(root, 'system/a.txt');
    const right = path.join(root, 'system/b.txt');
    await rename(left, `${left}.tmp`);
    await rename(right, left);
    await rename(`${left}.tmp`, right);
    await assert.rejects(loadWorld(root), (error) => error.code === 'corrupt_world' && ['sys.a', 'sys.b'].includes(error.details.leaf));
  });
});

test('world manifest seal、缺失 leaf 与多层引用错误失败并指出 id', async (t) => {
  const { root } = await sealedWorld(t, (builder) => {
    builder.addLeaf('sys.a', 'A');
    builder.addRecipe('sys.outer', [{ leaf: 'sys.a' }]);
  });
  const manifest = await readFile(path.join(root, 'manifest.json'));
  await writeFile(path.join(root, 'manifest.json'), Buffer.concat([manifest, Buffer.from('\n')]));
  await assert.rejects(loadWorld(root), { code: 'corrupt_world' });

  await t.test('malformed seal', async (subtest) => {
    const { root: innerRoot } = await sealedWorld(subtest, (builder) => builder.addLeaf('sys.a', 'A'));
    await writeFile(path.join(innerRoot, 'seal.json'), '{');
    await assert.rejects(loadWorld(innerRoot), { code: 'corrupt_world' });
  });

  await t.test('missing leaf file', async (subtest) => {
    const { root: innerRoot } = await sealedWorld(subtest, (builder) => {
      builder.addLeaf('sys.a', 'A');
      builder.addRecipe('sys.outer', [{ leaf: 'sys.a' }]);
    });
    const target = path.join(innerRoot, 'system/a.txt');
    await unlink(target);
    await assert.rejects(loadWorld(innerRoot), (error) => error.code === 'corrupt_world' && error.details.leaf === 'sys.a');
  });

  await t.test('cycle and unknown refs', async (subtest) => {
    const directory = await temporary(subtest);
    const builder = createWorldBuilder();
    builder.addRecipe('sys.a', [{ recipe: 'sys.b' }]);
    builder.addRecipe('sys.b', [{ recipe: 'sys.a' }]);
    await assert.rejects(builder.seal(path.join(directory, 'cycle')), { code: 'world_recipe_cycle' });

    const unknown = createWorldBuilder();
    unknown.addRecipe('sys.a', [{ leaf: 'sys.missing' }]);
    await assert.rejects(unknown.seal(path.join(directory, 'unknown')), { code: 'unknown_world_leaf' });
  });
});

test('Anthropic 与 Responses world 字段按原字节重建并校验 raw hash', async (t) => {
  const directory = await temporary(t);
  const builder = createWorldBuilder();
  const worldRoot = path.join(directory, 'world');
  const cases = [anthropicBody(), responsesBody()];
  const encoded = cases.map((body) => encodeRequestStub(body, { world: builder }));
  assert.ok(encoded[0].parts.some((part) => part.leaf));
  assert.ok(encoded[1].parts.some((part) => part.leaf));
  await builder.seal(worldRoot);
  const world = await loadWorld(worldRoot);
  for (const [index, stub] of encoded.entries()) {
    assert.equal(stub.legacy, undefined);
    assert.equal(stub.rawSha256, sha256(cases[index]));
    const decoded = await decodeRequestStub(stub, { world });
    assert.deepEqual(decoded.bytes, cases[index]);
  }
});

test('slots 可用语义 recipe 替换高频 system 段，并逐字节校验重建', async (t) => {
  const directory = await temporary(t);
  const builder = createWorldBuilder();
  const block1 = 'PREFIX\n\n## Skills\n\nSKILLS BODY\n\nTAIL';
  builder.addLeaf('sys.block1.prefix', 'PREFIX\n\n');
  builder.addLeaf('sys.skills', '## Skills\n\nSKILLS BODY');
  builder.addLeaf('sys.block1.tail', '\n\nTAIL');
  builder.addRecipe('sys.block1', [{ leaf: 'sys.block1.prefix' }, { leaf: 'sys.skills' }, { leaf: 'sys.block1.tail' }]);
  const body = Buffer.from(JSON.stringify({
    model: 'fixture', stream: true,
    system: [{ type: 'text', text: 'block0' }, { type: 'text', text: block1 }],
    messages: [{ role: 'user', content: 'question' }],
  }));
  const stub = encodeRequestStub(body, {
    world: builder,
    slots: (field) => (field.path === '/system/1/text' ? { recipe: 'sys.block1', as: 'json-string' } : null),
  });
  assert.equal(stub.legacy, undefined);
  assert.ok(stub.parts.some((part) => part.recipe === 'sys.block1'));
  await builder.seal(path.join(directory, 'world'));
  const world = await loadWorld(path.join(directory, 'world'));
  assert.deepEqual((await decodeRequestStub(stub, { world })).bytes, body);
});

test('unsafe number 与无法字节重建的转义文本退化为 legacy', async (t) => {
  const directory = await temporary(t);
  const builder = createWorldBuilder();
  const unsafe = encodeRequestStub(Buffer.from('{"system":"x","n":9007199254740993}'), { world: builder });
  assert.equal(unsafe.legacy, true);
  assert.equal(unsafe.reason, 'unsafe_number');
  const escaped = encodeRequestStub(Buffer.from('{"system":"a\\/b"}'), { world: builder });
  assert.equal(escaped.legacy, true);
  assert.equal(escaped.reason, 'text_encoding_mismatch');
  assert.equal(escaped.parts, undefined);
  await builder.seal(path.join(directory, 'world'));
  const world = await loadWorld(path.join(directory, 'world'));
  await assert.rejects(decodeRequestStub({ ...unsafe, parts: [{ leaf: 'sys.sha256.0000' }] }, { world }), { code: 'unknown_world_leaf' });
});

test('stub 引用缺少 world 时失败，raw hash 与长度受到校验', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-world-stub-'));
  try {
    const builder = createWorldBuilder();
    const body = anthropicBody();
    const stub = encodeRequestStub(body, { world: builder });
    await builder.seal(path.join(directory, 'world'));
    const world = await loadWorld(path.join(directory, 'world'));
    const worldRefs = collectWorldRefs(stub.parts, world);
    const broken = { ...stub, rawSha256: '0'.repeat(64) };
    await assert.rejects(decodeRequestStub(broken, { world, worldRefs }), { code: 'corrupt_cassette' });
    // 只有哈希字段变化、leaf 内容未变时，drift 无法解释，仍按损坏处理。
    await assert.rejects(decodeRequestStub(broken, { world, worldRefs }), (error) => error.code === 'corrupt_cassette');
    const missing = { ...stub, parts: stub.parts.map((part) => part.leaf ? { ...part, leaf: 'sys.sha256.missing' } : part) };
    await assert.rejects(decodeRequestStub(missing, { world, worldRefs }), { code: 'unknown_world_leaf' });
    await assert.rejects(decodeRequestStub(stub, {}), { code: 'missing_world' });
    await assert.rejects(decodeRequestStub({ ...stub, rawBytes: 1 }, { world, worldRefs }), { code: 'corrupt_cassette' });
    assert.equal(requestCoreSha256(stub), requestCoreSha256(JSON.parse(JSON.stringify(stub))));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('mapped leaf 变化可解释 drift，未映射 leaf 与结构变化必须失败', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'peri-world-drift-'));
  try {
    const builder = createWorldBuilder();
    builder.addLeaf('sys.skills', 'SKILLS-A');
    builder.addLeaf('sys.block0.main', 'BLOCK-STATIC');
    const body = Buffer.from(JSON.stringify({
      model: 'fixture-model', max_tokens: 32000,
      system: [{ type: 'text', text: 'SKILLS-A' }, { type: 'text', text: 'BLOCK-STATIC' }],
      messages: [],
    }));
    const slots = (field) => (field.path === '/system/0/text'
      ? { leaf: 'sys.skills' }
      : field.path === '/system/1/text' ? { leaf: 'sys.block0.main' } : null);
    const stub = encodeRequestStub(body, { world: builder, slots });
    await builder.seal(path.join(directory, 'world'));
    const world = await loadWorld(path.join(directory, 'world'));
    const worldRefs = collectWorldRefs(stub.parts, world);
    assert.deepEqual(worldRefs.map((ref) => ref.id), ['sys.skills', 'sys.block0.main']);

    // 只有 mapped leaf 允许变化。
    const skillsWorld = await rewriteWorldLeaf(path.join(directory, 'world'), path.join(directory, 'skills-world'), 'sys.skills', 'SKILLS-B');
    const drifted = await decodeRequestStub(stub, { world: skillsWorld, worldRefs });
    assert.equal(drifted.drifted, true);
    assert.deepEqual(drifted.driftLeaves, ['sys.skills']);
    assert.match(drifted.bytes.toString('utf8'), /SKILLS-B/u);

    // 未映射 leaf 变化必须失败。
    const blockWorld = await rewriteWorldLeaf(path.join(directory, 'world'), path.join(directory, 'block-world'), 'sys.block0.main', 'BLOCK-TAMPER');
    await assert.rejects(decodeRequestStub(stub, { world: blockWorld, worldRefs }),
      (error) => error.code === 'world_drift_unmapped_leaf' && error.details.leaf === 'sys.block0.main');

    // 录制的 leaf 序列与当前 recipe 结构不一致时失败。
    const reordered = { ...stub, parts: [stub.parts[0], stub.parts[3], stub.parts[2], stub.parts[1], stub.parts[4]] };
    await assert.rejects(decodeRequestStub(reordered, { world: skillsWorld, worldRefs }), { code: 'corrupt_cassette' });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('typed-template 压缩后展开与原 segments 逐字节一致，canonical body 不变', async () => {
  const body = anthropicBody();
  const { adapter, rules } = recordingAdapter(body);
  assert.equal(rules.length, 1);
  const compact = compactTypedTemplateRules(rules, { bindings: adapter.registry, owner: 'world', bodyBytes: body });
  assert.equal(compact[0].segments, undefined);
  assert.deepEqual(compact[0].pattern, [
    { kind: 'path', alias: 'work' }, { kind: 'date', alias: 'today' }, { kind: 'session', alias: 'session' },
  ]);
  const expanded = expandTypedTemplateRules(compact, body);
  assert.deepEqual(expanded.rules[0].segments, rules[0].segments);
  assert.equal(
    canonicalRecordedBody(body, expanded.rules, expanded.bindings),
    canonicalBody(body, { bindings: adapter.registry, owner: 'world', rules }),
  );
});

test('pattern 歧义、缺失、顺序与 patternSha 篡改均显式失败', () => {
  const text = { system: [{ type: 'text', text: 'a /record/root/work b /record/root/work c' }] };
  const raw = Buffer.from(JSON.stringify(text));
  const base = {
    path: '/system/0/text', mode: 'typed-template', systemText: true,
    pattern: [{ kind: 'path', alias: 'work' }],
    patternSha256: '0'.repeat(64),
    recorded: { 'path:work': '/record/root/work' },
  };
  assert.throws(() => expandTypedTemplateRules([base], raw), { code: 'binding_template_pattern_mismatch' });
  const { patternSha256, ...rest } = base;
  const ambiguous = { ...rest, patternSha256: sha256(canonicalJson(rest.pattern)) };
  assert.throws(() => expandTypedTemplateRules([ambiguous], raw), (error) => error.code === 'binding_template_ambiguous' && error.details.actual?.length === 2);

  const zeroText = Buffer.from(JSON.stringify({ system: [{ type: 'text', text: 'no matching value' }] }));
  assert.throws(() => expandTypedTemplateRules([ambiguous], zeroText), (error) => error.code === 'binding_template_ambiguous' && /一次也未出现/u.test(error.message));

  const repeated = {
    ...ambiguous,
    pattern: [{ kind: 'path', alias: 'work' }, { kind: 'path', alias: 'work' }],
    patternSha256: sha256(canonicalJson([{ kind: 'path', alias: 'work' }, { kind: 'path', alias: 'work' }])),
  };
  const repeatedSegments = expandTypedTemplateRules([repeated], raw).rules[0].segments;
  assert.deepEqual(repeatedSegments.filter((part) => typeof part !== 'string'), [{ kind: 'path', alias: 'work' }, { kind: 'path', alias: 'work' }]);

  const orderText = Buffer.from(JSON.stringify({ system: [{ type: 'text', text: '/b /a' }] }));
  const orderPattern = [{ kind: 'path', alias: 'a' }, { kind: 'path', alias: 'b' }];
  const orderRule = {
    path: '/system/0/text', mode: 'typed-template', systemText: true, pattern: orderPattern,
    patternSha256: sha256(canonicalJson(orderPattern)),
    recorded: { 'path:a': '/a', 'path:b': '/b' },
  };
  assert.throws(() => expandTypedTemplateRules([orderRule], orderText), { code: 'binding_template_ambiguous' });

  const extra = { ...ambiguous, recorded: { ...ambiguous.recorded, 'session:extra': 'session-recorded' } };
  assert.throws(() => expandTypedTemplateRules([extra], raw), { code: 'binding_template_ambiguous' });
});

test('live 与 recorded 不同时用实际 body 定位，并重建 recorded canonical', () => {
  const liveValue = '/live/root/work';
  const recordedValue = '/recorded/root/work';
  const body = Buffer.from(JSON.stringify({ system: [{ type: 'text', text: `cwd ${liveValue}` }], messages: [] }));
  const adapter = createRequestBindingAdapter({ mode: 'record', runId: 'record', declarations: [
    { owner: 'world', kind: 'path', alias: 'work', recorded: recordedValue, anchor: 'work' },
  ] });
  adapter.declareAndBind({ kind: 'path', alias: 'work', live: liveValue, anchor: 'work' });
  adapter.onRequest({ context: identity, request: { body } });
  const rules = adapter.requestRules(identity);
  const compact = compactTypedTemplateRules(rules, { bindings: adapter.registry, owner: 'world', bodyBytes: body });
  assert.deepEqual(compact[0].live, { 'path:work': liveValue });
  assert.deepEqual(compact[0].recorded, { 'path:work': recordedValue });
  const expanded = expandTypedTemplateRules(compact, body);
  const canonical = canonicalRecordedBody(body, expanded.rules, expanded.bindings);
  assert.equal(canonical, canonicalBody(body, { bindings: adapter.registry, owner: 'world', rules }));
  assert.match(canonical, /\/recorded\/root\/work/u);
  assert.doesNotMatch(canonical, /\/live\/root\/work/u);
});

test('非模板规则在 v2 中保留 live/recorded 并可重建 canonical', async () => {
  const body = Buffer.from(JSON.stringify({ prompt: '/record/root/work/file.txt' }));
  const adapter = createRequestBindingAdapter({ mode: 'record', runId: 'record' });
  bindWorld(adapter, { work: ['path', '/record/root/work'] });
  const rules = [{ path: '/prompt', mode: 'path-prefix', kind: 'path', alias: 'work' }];
  const canonical = canonicalBody(body, { bindings: adapter.registry, owner: 'world', rules: [] });
  const compact = compactTypedTemplateRules(rules, { bindings: adapter.registry, owner: 'world', bodyBytes: body });
  assert.equal(compact[0].live, '/record/root/work');
  assert.equal(compact[0].recorded, '/record/root/work');
  const expanded = expandTypedTemplateRules(compact, body);
  assert.equal(canonicalRecordedBody(body, expanded.rules, expanded.bindings), canonical);
});
