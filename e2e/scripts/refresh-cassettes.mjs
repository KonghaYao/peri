#!/usr/bin/env node
/**
 * world:sync —— 把高频 prompt 源文件同步进 replay world 快照。
 *
 * 只更新受源码映射的 leaf（skills、compact summary、Agent description/send_only）和 seal；
 * 其余 leaf 与 manifest 原样保留。源未变化时不写任何文件（幂等）。
 * 未映射（内容寻址）leaf 会在报告中列出，提醒审核而非静默忽略。
 *
 * --check 除源文件映射外，还与各 cassette seal 的录制 worldRefs 交叉校验：
 * world seal 只能证明快照自洽，手工重封无法解释未映射 leaf 的变化。
 */
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import {
  SOURCE_FILES, STABLE_LEAVES, agentDescriptionLeafIds, agentSchemaOf, expectedSendOnly,
} from './replay-world-sources.mjs';
import { compareRecordedWorldRefs, readRecordedWorldRefs } from './replay-world-recorded.mjs';
import { replaceWorldDirectory } from './world-sync-swap.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const load = (relative) => import(pathToFileURL(path.join(root, relative)));
const { sha256 } = await load('e2e/llm-replay/matcher.mjs');
const { WORLD_FORMAT_VERSION, loadWorld } = await load('e2e/llm-replay/world-store.mjs');

const { values } = parseArgs({
  options: {
    check: { type: 'boolean', default: false },
    world: { type: 'string' },
    json: { type: 'boolean', default: true },
  },
});

const worldDir = path.resolve(values.world ?? path.join(root, 'e2e/fixtures/replay-v2/world'));
const world = await loadWorld(worldDir);

function requiresLeaf(id) {
  if (!world.hasLeaf(id)) throw new Error(`world 缺少稳定 leaf ${id}，无法映射对应源文件`);
}

const source = {
  skills: await readFile(path.join(root, SOURCE_FILES.skills), 'utf8'),
  compactSummary: await readFile(path.join(root, SOURCE_FILES.compactSummary), 'utf8'),
  agentDescription: await readFile(path.join(root, SOURCE_FILES.agentDescription), 'utf8'),
  agentSchema: await readFile(path.join(root, SOURCE_FILES.agentSchema), 'utf8'),
};

requiresLeaf(STABLE_LEAVES.skills);
requiresLeaf(STABLE_LEAVES.compactSummary);

function recipeReferences(parts, leaf, seen = new Set()) {
  return parts.some((part) => {
    if (part.leaf === leaf) return true;
    if (!part.recipe || seen.has(part.recipe)) return false;
    return recipeReferences(world.manifest.recipes[part.recipe] ?? [], leaf, new Set([...seen, part.recipe]));
  });
}
const skillsReferenced = Object.values(world.manifest.recipes)
  .some((parts) => recipeReferences(parts, STABLE_LEAVES.skills));
if (!skillsReferenced) throw new Error('world 中没有 recipe 引用 sys.skills，skills 变化不会进入请求');

const updates = new Map([
  [STABLE_LEAVES.skills, Buffer.from(source.skills, 'utf8')],
  [STABLE_LEAVES.compactSummary, Buffer.from(source.compactSummary, 'utf8')],
]);

// Agent description 与 send_only 共享同一 tool leaf；带参数化 schema 的 provider 也一并更新。
const sendOnly = expectedSendOnly(source.agentSchema);
for (const id of agentDescriptionLeafIds(world)) {
  const current = world.leafBytes(id).toString('utf8');
  let tool;
  try { tool = JSON.parse(current); }
  catch { throw new Error(`${id}: leaf 不是合法 JSON，拒绝改写`); }
  if (JSON.stringify(tool) !== current) throw new Error(`${id}: leaf JSON 无法无损往返，拒绝改写`);
  tool.description = source.agentDescription;
  agentSchemaOf(tool).properties.send_only = structuredClone(sendOnly);
  const next = JSON.stringify(tool);
  if (next !== current) updates.set(id, Buffer.from(next, 'utf8'));
}

const changed = [...updates].filter(([id, bytes]) => !bytes.equals(world.leafBytes(id)));
const unmappedLeaves = world.leaves.filter((id) => id.startsWith('sys.sha256.') || id.startsWith('tools.sha256.'));
const report = {
  updated: changed.map(([id]) => id),
  stable: updates.size - changed.length,
  unmappedLeaves,
  world: { leaves: world.leaves.length, recipes: world.recipes.length, generation: world.manifest.generation },
};

if (values.check) {
  const recorded = compareRecordedWorldRefs(world, await readRecordedWorldRefs(path.dirname(worldDir)));
  const staleReasons = [];
  if (changed.length) staleReasons.push('source_not_synced');
  if (recorded.unmappedDrift.length) staleReasons.push('cassette_unmapped_drift');
  if (recorded.missingLeaves.length) staleReasons.push('cassette_missing_leaf');
  const stale = staleReasons.length > 0;
  console.log(JSON.stringify({
    ...report,
    recorded: { ...recorded, requiresRerecord: recorded.mappedDrift.length > 0 },
    staleReasons,
    stale,
  }, null, 2));
  process.exit(stale ? 1 : 0);
}
if (!changed.length) {
  console.log(JSON.stringify({ ...report, written: 0 }, null, 2));
  process.exit(0);
}

const stagingBase = await mkdtemp(path.join(path.dirname(worldDir), '.world-sync-'));
const staged = path.join(stagingBase, 'world');
try {
  await mkdir(staged);
  const leafHashes = {};
  for (const [id, meta] of Object.entries(world.manifest.leaves)) {
    const bytes = updates.get(id) ?? world.leafBytes(id);
    const file = path.join(staged, meta.file);
    await mkdir(path.dirname(file), { recursive: true });
    await writeFile(file, bytes, { mode: 0o600 });
    leafHashes[id] = sha256(bytes);
  }
  const manifestBytes = await readFile(path.join(worldDir, 'manifest.json'));
  await writeFile(path.join(staged, 'manifest.json'), manifestBytes, { mode: 0o600 });
  const seal = {
    formatVersion: WORLD_FORMAT_VERSION,
    manifestSha256: sha256(manifestBytes),
    leaves: Object.fromEntries(Object.entries(leafHashes).sort(([left], [right]) => left.localeCompare(right))),
  };
  await writeFile(path.join(staged, 'seal.json'), `${JSON.stringify(seal)}\n`, { mode: 0o600 });
  await loadWorld(staged);
  const swap = await replaceWorldDirectory(worldDir, staged, { validate: (dir) => loadWorld(dir) });
  if (!swap.removed) console.error(`world 已替换，但旧目录清理失败：${swap.previous}`);
} finally {
  await rm(stagingBase, { recursive: true, force: true });
}
console.log(JSON.stringify({ ...report, written: changed.length }, null, 2));
