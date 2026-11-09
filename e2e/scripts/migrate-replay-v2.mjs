#!/usr/bin/env node
/**
 * 一次性迁移：replay-v2 fixture 从 cassette formatVersion 1 迁到 world 快照 + v2 stub。
 *
 * 迁移只做字节级重排：world leaf + recipe 渲染出的请求必须与旧 raw body 逐字节相等，
 * typed-template 展开后的 segments 与旧 rules 逐字节相等，canonical body 与旧值逐字节相等。
 * 任一断言失败即整批中止，staging 目录不会替换现有 fixture。
 */
import { mkdir, mkdtemp, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { SOURCE_FILES, STABLE_LEAVES, classifyBlock1Prefix } from './replay-world-sources.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const fixtures = path.join(root, 'e2e/fixtures/replay-v2');
const worldDir = path.join(fixtures, 'world');
const load = (relative) => import(pathToFileURL(path.join(root, relative)));
const { loadCassette } = await load('e2e/llm-replay/cassette.mjs');
const { createBindingRegistry } = await load('e2e/llm-replay/bindings.mjs');
const { identityKey, sha256 } = await load('e2e/llm-replay/matcher.mjs');
const {
  CASSETTE_FORMAT_VERSION, canonicalRecordedBody, compactTypedTemplateRules,
  createWorldBuilder, encodeRequestStub, expandTypedTemplateRules, loadWorld,
  parseRequestSpans, serializeRequestStub,
} = await load('e2e/llm-replay/world-store.mjs');

const { values } = parseArgs({
  options: { 'dry-run': { type: 'boolean', default: false }, json: { type: 'boolean', default: true } },
});

const assert = (condition, message) => { if (!condition) throw new Error(message); };
const exists = async (file) => readFile(file).then(() => true, () => false);
const text = (bytes) => bytes.toString('utf8');
const sameJson = (left, right) => JSON.stringify(left) === JSON.stringify(right);

async function treeStats(directory) {
  let files = 0;
  let bytes = 0;
  const walk = async (dir) => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory()) await walk(file);
      else if (entry.isFile()) { files += 1; bytes += (await readFile(file)).length; }
    }
  };
  await walk(directory);
  return { files, bytes };
}

const ids = [];
for (const entry of await readdir(fixtures, { withFileTypes: true })) {
  if (!entry.isDirectory() || entry.name.startsWith('.')) continue;
  try {
    await readFile(path.join(fixtures, entry.name, 'cassette', 'manifest.json'));
    ids.push(entry.name);
  } catch { /* world/ 等非 cassette 目录 */ }
}
ids.sort();

// 已是 v2：直接跳过，保证生成器可重复运行。
if (await exists(path.join(worldDir, 'manifest.json'))) {
  const existing = JSON.parse(await readFile(path.join(worldDir, 'manifest.json'), 'utf8'));
  if (existing.formatVersion === CASSETTE_FORMAT_VERSION) {
    console.log(JSON.stringify({ skipped: true, reason: 'already-v2' }));
    process.exit(0);
  }
}

const cassettes = [];
for (const id of ids) {
  const directory = path.join(fixtures, id, 'cassette');
  const cassette = await loadCassette(directory);
  const scenario = JSON.parse(await readFile(path.join(fixtures, id, 'scenario.json'), 'utf8'));
  cassettes.push({ id, directory, cassette, scenario });
}

const versions = new Set(cassettes.map((item) => item.cassette.manifest.formatVersion));
assert(versions.size === 1 && versions.has(1), `cassette 格式不一致或不是 v1: ${[...versions].join(',')}`);
assert(!(await exists(worldDir)), 'cassette 仍是 v1，但 world/ 已存在；请先清理迁移残留');

const skillsSource = await readFile(path.join(root, SOURCE_FILES.skills), 'utf8');
const compactSummary = await readFile(path.join(root, SOURCE_FILES.compactSummary), 'utf8');
const intro = 'You are an interactive CLI tool that helps users with software engineering tasks.';

const requests = [];
for (const item of cassettes) {
  for (const entry of item.cassette.manifest.entries) {
    const raw = await readFile(path.join(item.directory, entry.request.file));
    requests.push({ cassette: item, entry, raw, body: JSON.parse(text(raw)) });
  }
}

// block0 是唯一的高频基础段落；出现多个变体时说明 prompt 组装已变化，迁移必须显式停下。
const block0Counts = new Map();
for (const request of requests) {
  for (const part of Array.isArray(request.body.system) ? request.body.system : []) {
    if (typeof part.text === 'string' && part.text.startsWith(intro)) {
      block0Counts.set(part.text, (block0Counts.get(part.text) ?? 0) + 1);
    }
  }
}
assert(block0Counts.size === 1, `block0 变体数量异常: ${block0Counts.size}`);
const block0 = [...block0Counts.keys()][0];

const builder = createWorldBuilder();
builder.addLeaf(STABLE_LEAVES.block0, block0);
builder.addLeaf(STABLE_LEAVES.skills, skillsSource);
builder.addLeaf(STABLE_LEAVES.compactSummary, compactSummary);

function toolsSpan(raw) {
  const bodyText = text(raw);
  const tree = parseRequestSpans(bodyText);
  const field = tree.entries?.find((item) => item.key === 'tools');
  if (!field) return null;
  const slice = bodyText.slice(field.value.start, field.value.end);
  const span = parseRequestSpans(slice);
  return { slice, items: span.items.map((item) => slice.slice(item.start, item.end)) };
}

const toolContexts = new Map();
const toolVariants = new Map();
for (const request of requests) {
  if (!request.body.tools) continue;
  const { slice, items } = toolsSpan(request.raw);
  const first = JSON.parse(items[0]);
  const provider = Object.hasOwn(first, 'input_schema') ? 'anthropic' : Object.hasOwn(first, 'parameters') ? 'responses' : null;
  assert(provider, `${request.cassette.id}: 无法识别 tools 协议形状`);
  const role = items.some((item) => JSON.parse(item).name === 'Agent') ? 'main' : 'sub';
  const context = `${provider}.${role}`;
  const existing = toolContexts.get(context);
  if (existing) assert(existing.slice === slice, `${request.cassette.id}: tools 上下文 ${context} 出现不同内容`);
  else toolContexts.set(context, { slice, items });
  for (const item of items) {
    const name = JSON.parse(item).name;
    if (!toolVariants.has(name)) toolVariants.set(name, new Map());
    const variants = toolVariants.get(name);
    if (!variants.has(item)) variants.set(item, new Set());
    variants.get(item).add(context);
  }
}
assert(toolContexts.size > 0, '没有发现任何 tools 字段');

const allProviders = new Set([...toolVariants.values()]
  .flatMap((variants) => [...variants.values()].flatMap((contexts) => [...contexts].map((context) => context.split('.')[0]))));
const toolLeafByContent = new Map();
for (const [name, variants] of toolVariants) {
  const providerVariantCounts = new Map();
  for (const contexts of variants.values()) {
    const providers = new Set([...contexts].map((context) => context.split('.')[0]));
    assert(providers.size === 1, `${name}: 同一工具内容出现在多个协议`);
    const provider = [...providers][0];
    providerVariantCounts.set(provider, (providerVariantCounts.get(provider) ?? 0) + 1);
  }
  for (const [content, contexts] of variants) {
    const provider = [...new Set([...contexts].map((context) => context.split('.')[0]))][0];
    const roles = new Set([...contexts].map((context) => context.split('.')[1]));
    const parts = ['tools', name];
    if (allProviders.size > 1) parts.push(provider);
    // 同一协议下存在多个变体时才需要角色后缀；共享内容（如 Bash）不重复区分。
    if (providerVariantCounts.get(provider) > 1) {
      assert(roles.size === 1, `${name}: 同一内容对应多个角色`);
      parts.push([...roles][0]);
    }
    const leafId = parts.join('.');
    assert(!toolLeafByContent.has(content) || toolLeafByContent.get(content) === leafId, `${name}: 同一内容生成了不同 leaf id`);
    toolLeafByContent.set(content, leafId);
    builder.addLeaf(leafId, content);
  }
}

const toolRecipeBySlice = new Map();
for (const [context, { slice, items }] of toolContexts) {
  const parts = [];
  let cursor = 0;
  for (const item of items) {
    const at = slice.indexOf(item, cursor);
    assert(at >= 0, `tools.${context}: 无法按原字节切分工具对象`);
    if (at > cursor) parts.push(slice.slice(cursor, at));
    parts.push({ leaf: toolLeafByContent.get(item) });
    cursor = at + item.length;
  }
  if (cursor < slice.length) parts.push(slice.slice(cursor));
  const recipeId = `tools.${context}`;
  builder.addRecipe(recipeId, parts);
  toolRecipeBySlice.set(slice, recipeId);
}

const prefixNameByContent = new Map();
const prefixContentByName = new Map();
const tailIdsByPrefix = new Map();
const block1Recipes = new Set();
const textToBlock1 = new Map();
const responsesRecipes = new Set();
const textToResponses = new Map();

function registerBlock1(prefix, tail) {
  let name = prefixNameByContent.get(prefix);
  if (!name) {
    name = classifyBlock1Prefix(prefix);
    const existing = prefixContentByName.get(name);
    if (existing !== undefined && existing !== prefix) throw new Error(`block1 prefix 分类冲突: ${name}`);
    prefixNameByContent.set(prefix, name);
    prefixContentByName.set(name, prefix);
    builder.addLeaf(`sys.block1.${name}.prefix`, prefix);
  }
  if (!tailIdsByPrefix.has(name)) tailIdsByPrefix.set(name, new Map());
  const tails = tailIdsByPrefix.get(name);
  let tailId = tails.get(tail);
  if (!tailId) {
    tailId = `t${String(tails.size + 1).padStart(2, '0')}`;
    tails.set(tail, tailId);
    builder.addLeaf(`sys.block1.${name}.${tailId}.tail`, tail);
  }
  const recipeId = `sys.block1.${name}.${tailId}`;
  if (!block1Recipes.has(recipeId)) {
    builder.addRecipe(recipeId, [
      { leaf: `sys.block1.${name}.prefix` },
      { leaf: STABLE_LEAVES.skills },
      { leaf: `sys.block1.${name}.${tailId}.tail` },
    ]);
    block1Recipes.add(recipeId);
  }
  return { recipeId, name, tailId };
}

function responsesRecipe(value) {
  if (!value.startsWith(block0)) return null;
  const rest = value.slice(block0.length);
  const at = rest.indexOf(skillsSource);
  if (at < 0 || rest.indexOf(skillsSource, at + skillsSource.length) >= 0) return null;
  const beforeSkills = rest.slice(0, at);
  const tail = rest.slice(at + skillsSource.length);
  const candidates = [...prefixContentByName.entries()].filter(([, prefix]) => beforeSkills.endsWith(prefix));
  if (candidates.length !== 1) return null;
  const [name, prefix] = candidates[0];
  const separator = beforeSkills.slice(0, beforeSkills.length - prefix.length);
  if (!/^[\x20\t\r\n]*$/u.test(separator)) return null;
  const block1 = registerBlock1(prefix, tail);
  const recipeId = `sys.responses.${name}.${block1.tailId}`;
  if (!responsesRecipes.has(recipeId)) {
    const parts = [{ leaf: STABLE_LEAVES.block0 }];
    if (separator) parts.push(separator);
    parts.push({ recipe: block1.recipeId });
    builder.addRecipe(recipeId, parts);
    responsesRecipes.add(recipeId);
  }
  return recipeId;
}

const unmapped = [];
let legacy = 0;
const rulesCompared = [];
for (const request of requests) {
  const { cassette, entry, raw } = request;
  const bodyText = text(raw);
  const slots = (field) => {
    if (field.kind === 'tools') {
      const slice = bodyText.slice(field.start, field.end);
      const recipeId = toolRecipeBySlice.get(slice);
      return recipeId ? { recipe: recipeId } : null;
    }
    if (field.kind !== 'system' && field.kind !== 'responses') return null;
    const value = field.text;
    if (value === block0) return { leaf: STABLE_LEAVES.block0 };
    if (value === compactSummary) return { leaf: STABLE_LEAVES.compactSummary };
    if (textToBlock1.has(value)) return { recipe: textToBlock1.get(value) };
    if (textToResponses.has(value)) return { recipe: textToResponses.get(value) };
    const at = value.indexOf(skillsSource);
    if (at >= 0) {
      if (value.indexOf(skillsSource, at + skillsSource.length) >= 0) {
        throw new Error(`${cassette.id}: skills 段在同一 system 文本中出现多次`);
      }
      if (field.kind === 'system') {
        const block1 = registerBlock1(value.slice(0, at), value.slice(at + skillsSource.length));
        textToBlock1.set(value, block1.recipeId);
        return { recipe: block1.recipeId };
      }
      const recipeId = responsesRecipe(value);
      if (recipeId) {
        textToResponses.set(value, recipeId);
        return { recipe: recipeId };
      }
    }
    unmapped.push({
      cassette: cassette.id, call: entry.identity.call, path: field.path, kind: field.kind,
      reason: at >= 0 ? 'unrecognized_composition' : 'no_skills', sha256: sha256(value),
    });
    return null;
  };

  const scenarioBindings = cassette.scenario.bindings ?? [];
  const registry = createBindingRegistry({ runId: `migrate-${cassette.id}`, declarations: scenarioBindings });
  for (const binding of scenarioBindings) registry.bind({ ...binding, live: binding.recorded });
  const oldRules = entry.requestRules ?? [];
  const compact = compactTypedTemplateRules(oldRules, { bindings: registry, owner: 'world', bodyBytes: raw });
  for (const rule of compact) {
    if (rule.live && rule.recorded && sameJson(rule.live, rule.recorded)) delete rule.live;
  }
  const expanded = expandTypedTemplateRules(compact, raw);
  const oldSegments = oldRules.map((rule) => rule.segments);
  const newSegments = expanded.rules.map((rule) => rule.segments);
  if (!sameJson(oldSegments, newSegments)) {
    throw new Error(`${cassette.id}/${entry.identity.call}: typed-template 展开与旧 segments 不一致`);
  }
  rulesCompared.push(...oldSegments);
  const canonical = canonicalRecordedBody(raw, expanded.rules, expanded.bindings);
  if (canonical !== entry.request.canonicalBody) {
    throw new Error(`${cassette.id}/${entry.identity.call}: canonical body 重建不一致`);
  }
  const stub = encodeRequestStub(raw, { world: builder, slots });
  if (stub.legacy) {
    legacy += 1;
    throw new Error(`${cassette.id}/${entry.identity.call}: 请求无法字节重建 (${stub.reason})，拒绝迁移`);
  }
  if (stub.rawSha256 !== entry.request.sha256) {
    throw new Error(`${cassette.id}/${entry.identity.call}: rawSha256 与旧请求不一致`);
  }
  stub.canonicalSha256 = sha256(canonical);
  request.stub = stub;
  request.compact = compact;
  request.canonical = canonical;
}

assert(legacy === 0, `存在 ${legacy} 条 legacy 请求`);

const dryRun = values['dry-run'];
const stagingBase = await mkdtemp(path.join(dryRun ? os.tmpdir() : fixtures, dryRun ? 'peri-replay-migration-' : '.migration-'));
let report;
try {
  const stagedWorldDir = path.join(stagingBase, 'world');
  await builder.seal(stagedWorldDir);
  const stagedWorld = await loadWorld(stagedWorldDir);
  const worldBytes = (await treeStats(stagedWorldDir)).bytes;
  let finalWorldBytes = worldBytes;

  if (!dryRun) {
    const stagedCassettes = new Map();
    for (const cassette of cassettes) {
      const output = path.join(stagingBase, 'cassettes', cassette.id, 'cassette');
      await mkdir(path.join(output, 'requests'), { recursive: true });
      await mkdir(path.join(output, 'responses'), { recursive: true });
      const manifestEntries = [];
      const sealedFiles = {};
      for (const request of requests.filter((item) => item.cassette === cassette)) {
        const id = sha256(identityKey(request.entry.identity));
        const stubFile = `requests/${id}.stub.json`;
        const responseFile = `responses/${id}.bin`;
        assert(request.entry.response.file === responseFile, `${cassette.id}: 旧响应路径不符合身份`);
        const stubBytes = serializeRequestStub(request.stub);
        await writeFile(path.join(output, stubFile), stubBytes, { mode: 0o600 });
        sealedFiles[stubFile] = sha256(stubBytes);
        await writeFile(path.join(output, responseFile), await readFile(path.join(cassette.directory, request.entry.response.file)), { mode: 0o600 });
        manifestEntries.push({
          identity: request.entry.identity,
          requestRules: request.compact,
          request: {
            method: request.entry.request.method,
            target: request.entry.request.target,
            headers: request.entry.request.headers,
            stub: stubFile,
          },
          response: request.entry.response,
        });
      }
      const manifest = {
        formatVersion: CASSETTE_FORMAT_VERSION,
        kind: 'raw-http',
        provenance: cassette.cassette.manifest.provenance,
        entries: manifestEntries.sort((left, right) => identityKey(left.identity).localeCompare(identityKey(right.identity))),
      };
      const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
      await writeFile(path.join(output, 'manifest.json'), manifestBytes, { mode: 0o600 });
      const seal = {
        formatVersion: CASSETTE_FORMAT_VERSION,
        manifestSha256: sha256(manifestBytes),
        files: Object.fromEntries(Object.entries(sealedFiles).sort(([left], [right]) => left.localeCompare(right))),
      };
      await writeFile(path.join(output, 'seal.json'), `${JSON.stringify(seal)}\n`, { mode: 0o600 });
      stagedCassettes.set(cassette.id, output);
    }

    // 用 staging world 重新加载全部 cassette；canonical 与 segments 再次逐条核对。
    for (const cassette of cassettes) {
      const staged = await loadCassette(stagedCassettes.get(cassette.id), { worldDir: stagedWorldDir });
      assert(staged.report.legacy.length === 0, `${cassette.id}: staging 出现 legacy`);
      assert(staged.entries.size === cassette.cassette.entries.size, `${cassette.id}: staging entry 数量变化`);
      for (const [key, stagedEntry] of staged.entries) {
        const oldEntry = cassette.cassette.entries.get(key);
        assert(oldEntry, `${cassette.id}: staging 出现未知身份`);
        assert(stagedEntry.request.canonicalBody === oldEntry.request.canonicalBody, `${cassette.id}: staging canonical 不一致`);
        assert(sameJson(stagedEntry.requestRules.map((rule) => rule.segments), oldEntry.requestRules.map((rule) => rule.segments)),
          `${cassette.id}: staging segments 不一致`);
        assert(stagedEntry.response.sha256 === oldEntry.response.sha256, `${cassette.id}: staging response 哈希变化`);
      }
    }

    await rename(stagedWorldDir, worldDir);
    const backups = [];
    try {
      for (const [id, staged] of stagedCassettes) {
        const target = path.join(fixtures, id, 'cassette');
        const backup = path.join(stagingBase, 'backup', id, 'cassette');
        await mkdir(path.dirname(backup), { recursive: true });
        await rename(target, backup);
        await rename(staged, target);
        backups.push({ target, backup });
      }
    } catch (error) {
      for (const { target, backup } of backups.reverse()) {
        await rm(target, { recursive: true, force: true });
        await rename(backup, target).catch(() => {});
      }
      await rm(worldDir, { recursive: true, force: true });
      throw error;
    }
    finalWorldBytes = (await treeStats(worldDir)).bytes;
    // 备份目录位于 fixtures 内，统计体积前先移除，避免把旧 fixture 计入。
    if (!process.env.PERI_KEEP_MIGRATION) await rm(stagingBase, { recursive: true, force: true });
  }

  let entryCount = 0;
  let ruleCount = 0;
  let legacyCount = 0;
  for (const cassette of cassettes) {
    const loaded = dryRun ? cassette.cassette : await loadCassette(cassette.directory);
    entryCount += loaded.entries.size;
    ruleCount += [...loaded.entries.values()].reduce((sum, entry) => sum + entry.requestRules.length, 0);
    legacyCount += loaded.report.legacy.length;
  }
  const finalWorld = dryRun ? stagedWorld : await loadWorld(worldDir);
  const stats = await treeStats(fixtures);
  report = {
    migrated: !dryRun,
    formatVersion: dryRun ? 1 : CASSETTE_FORMAT_VERSION,
    cassettes: cassettes.length,
    entries: entryCount,
    typedTemplates: ruleCount,
    rulesCompared: rulesCompared.length,
    legacy: legacyCount,
    unmapped,
    world: {
      leaves: finalWorld.leaves.length,
      recipes: finalWorld.recipes.length,
      generation: finalWorld.manifest.generation,
      bytes: finalWorldBytes,
    },
    fixtureBytes: stats.bytes,
    fixtureFiles: stats.files,
  };
} finally {
  if (!process.env.PERI_KEEP_MIGRATION) await rm(stagingBase, { recursive: true, force: true });
  else console.error('kept staging at', stagingBase);
}
console.log(JSON.stringify(report, null, values.json ? 2 : 0));

