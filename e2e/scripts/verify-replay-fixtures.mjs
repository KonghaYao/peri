#!/usr/bin/env node
/** 校验 replay-v2 fixture 的 world seal 与全部 cassette；输出计数供 CI/PR 引用。 */
import { access, readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const fixtures = path.join(root, 'e2e/fixtures/replay-v2');
const load = (relative) => import(pathToFileURL(path.join(root, relative)));
const { loadCassette } = await load('e2e/llm-replay/cassette.mjs');
const { loadWorld } = await load('e2e/llm-replay/world-store.mjs');

const world = await loadWorld(path.join(fixtures, 'world'));
const ids = (await readdir(fixtures, { withFileTypes: true }))
  .filter((entry) => entry.isDirectory() && !entry.name.startsWith('.'))
  .map((entry) => entry.name)
  .sort();

const cassettes = [];
let entries = 0;
let legacy = 0;
let worldDrift = 0;
const formatVersions = new Set();
for (const id of ids) {
  const directory = path.join(fixtures, id, 'cassette');
  try { await access(path.join(directory, 'manifest.json')); }
  catch { continue; }
  const cassette = await loadCassette(directory);
  cassettes.push(id);
  entries += cassette.entries.size;
  legacy += cassette.report.legacy.length;
  worldDrift += cassette.report.worldDrift.length;
  formatVersions.add(cassette.manifest.formatVersion);
}

const report = {
  passed: legacy === 0 && worldDrift === 0 && formatVersions.size === 1 && formatVersions.has(2),
  cassettes: cassettes.length,
  entries,
  legacy,
  worldDrift,
  formatVersions: [...formatVersions].sort(),
  world: { leaves: world.leaves.length, recipes: world.recipes.length, generation: world.manifest.generation },
};
console.log(JSON.stringify(report, null, 2));
if (!report.passed) process.exitCode = 1;
