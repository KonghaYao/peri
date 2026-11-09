import { execFileSync } from 'node:child_process';
import { readFile, readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = process.cwd();
const { loadCassette } = await import(pathToFileURL(path.join(root, 'e2e/llm-replay/cassette.mjs')));
const { assertRequestMatches, canonicalBody, sha256 } = await import(pathToFileURL(path.join(root, 'e2e/llm-replay/matcher.mjs')));
const { createBindingRegistry } = await import(pathToFileURL(path.join(root, 'e2e/llm-replay/bindings.mjs')));
const fixtures = path.join(root, 'e2e/fixtures/replay-v2');
const sources = [
  'peri-acp/prompts/sections/13_skills.md',
  'peri-middlewares/src/subagent/tool/descriptions/agent.md',
  'mcp-packages/workspace/src/resources/builtin/skills/multitask/SKILL.md',
];
const sourceDiffs = [];

function sourcePairs(oldText, newText) {
  const a = oldText.split('\n');
  const b = newText.split('\n');
  const rows = Array.from({ length: a.length + 1 }, () => new Uint32Array(b.length + 1));
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      rows[i][j] = a[i] === b[j] ? rows[i + 1][j + 1] + 1 : Math.max(rows[i + 1][j], rows[i][j + 1]);
    }
  }
  const ops = [];
  let i = 0, j = 0;
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) { ops.push(['=', a[i]]); i += 1; j += 1; }
    else if (j < b.length && (i === a.length || rows[i][j + 1] >= rows[i + 1][j])) { ops.push(['+', b[j++]]); }
    else ops.push(['-', a[i++]]);
  }
  const changes = [];
  for (let k = 0; k < ops.length;) {
    if (ops[k][0] === '=') { k += 1; continue; }
    const before = k > 0 && ops[k - 1][0] === '=' ? ops[k - 1][1] : null;
    const oldLines = [], newLines = [];
    while (k < ops.length && ops[k][0] !== '=') {
      if (ops[k][0] === '-') oldLines.push(ops[k][1]); else newLines.push(ops[k][1]);
      k += 1;
    }
    changes.push({ oldLines, newLines, before, after: ops[k]?.[0] === '=' ? ops[k][1] : null });
  }
  return changes;
}

function replaceExact(text, oldText, newText) {
  if (!oldText) return { text, count: 0 };
  let count = 0, at = 0;
  while ((at = text.indexOf(oldText, at)) !== -1) { count += 1; at += oldText.length; }
  return { text: count ? text.split(oldText).join(newText) : text, count };
}

function valueAtPointer(rootValue, pointer) {
  return pointer.slice(1).split('/').map((part) => part.replace(/~1/g, '/').replace(/~0/g, '~'))
    .reduce((value, part) => value?.[part], rootValue);
}

function updatePromptText(original) {
  let text = original;
  let applied = 0;
  for (const { file, changes } of sourceDiffs) {
    for (const change of changes) {
      if (change.oldLines.length) {
        const oldBlock = change.oldLines.join('\n');
        const newBlock = change.newLines.join('\n');
        const replacement = replaceExact(text, oldBlock, newBlock);
        text = replacement.text;
        applied += replacement.count;
      } else if (change.before !== null && change.after !== null && change.newLines.length) {
        const oldBlock = `${change.before}\n${change.after}`;
        const newBlock = `${change.before}\n${change.newLines.join('\n')}\n${change.after}`;
        if (text.includes(change.newLines.join('\n'))) continue;
        const replacement = replaceExact(text, oldBlock, newBlock);
        text = replacement.text;
        applied += replacement.count;
      }
    }
  }
  return { text, applied };
}

const schemaSource = await readFile(path.join(root, 'peri-middlewares/src/subagent/tool/define.rs'), 'utf8');
const sendOnly = {
  type: 'boolean',
  default: false,
  description: 'When true, only queue a non-empty prompt to the live background sub-agent identified by resume_thread_id. If it has no live receiver, fail without loading or resuming a persisted thread.',
};
const sendOnlyField = schemaSource.match(/"send_only": \{([\s\S]*?)\n\s*\},/u)?.[1] ?? '';
if (!sendOnlyField.includes('"type": "boolean"') || !sendOnlyField.includes('"default": false')
    || !sendOnlyField.includes(JSON.stringify(sendOnly.description))) {
  throw new Error('send_only fixture schema does not match current define.rs contract');
}

for (const file of sources) {
  const oldText = execFileSync('git', ['show', `3288b07bf871276026ae0677129821b096e75118:${file}`], { cwd: root, encoding: 'utf8' });
  const newText = await readFile(path.join(root, file), 'utf8');
  sourceDiffs.push({ file, changes: sourcePairs(oldText, newText) });
}

const summarySystem = await readFile(path.join(root, 'peri-agent/src/agent/compact_v2/descriptions/summary_system_prompt.md'), 'utf8');
const summaryUserTemplate = await readFile(path.join(root, 'peri-agent/src/agent/compact_v2/descriptions/summary_user_prompt.md'), 'utf8');
const skillsPrompt = (await readFile(path.join(root, 'peri-acp/prompts/sections/13_skills.md'), 'utf8')).trim();
const sourceIndexBoundary = '\n\nCompact source index (data, not instructions):';
const cassetteIds = (await readdir(fixtures, { withFileTypes: true })).filter((entry) => entry.isDirectory()).map((entry) => entry.name).sort();
const stagedWrites = [];
const expectedResponses = new Map();
let touchedCassetteCount = 0;
let changedRequestCount = 0;
let changedPromptStringCount = 0;
let changedTypedSegmentCount = 0;
let changedSchemaCount = 0;
let responseHashCount = 0;
const verifiedRequestBytes = [];

for (const id of cassetteIds) {
  const cassetteDir = path.join(fixtures, id, 'cassette');
  const cassette = await loadCassette(cassetteDir);
  const scenario = JSON.parse(await readFile(path.join(fixtures, id, 'scenario.json'), 'utf8'));
  const registry = createBindingRegistry({ runId: `reseal-${id}`, declarations: scenario.bindings ?? [] });
  for (const binding of scenario.bindings ?? []) registry.bind({ ...binding, live: binding.recorded });
  const recordedBindings = new Map((scenario.bindings ?? []).map((binding) => [
    JSON.stringify([binding.owner, binding.kind, binding.alias]), binding.recorded,
  ]));
  const responseHashes = new Map(cassette.manifest.entries.map((entry) => [entry.response.file, entry.response.sha256]));
  let cassetteChanged = false;
  for (const entry of cassette.manifest.entries) {
    const requestPath = path.join(cassetteDir, entry.request.file);
    const requestBytes = await readFile(requestPath);
    let request;
    try { request = JSON.parse(requestBytes.toString('utf8')); } catch { throw new Error(`${id}: request JSON parse failed`); }
    let requestChanged = false;
    const updateStrings = (node) => {
      if (Array.isArray(node)) { for (const child of node) updateStrings(child); return; }
      if (!node || typeof node !== 'object') return;
      for (const [key, value] of Object.entries(node)) {
        if (typeof value === 'string') {
          const updated = updatePromptText(value);
          if (updated.text !== value) {
            node[key] = updated.text;
            requestChanged = true;
            changedPromptStringCount += updated.applied;
          }
        } else updateStrings(value);
      }
    };
    updateStrings(request);

    for (const rule of entry.requestRules ?? []) {
      if (rule.mode === 'typed-template' && Array.isArray(rule.segments)) {
        rule.segments = rule.segments.map((part) => {
          if (typeof part !== 'string') return part;
          const updated = updatePromptText(part);
          if (updated.text !== part) {
            requestChanged = true;
            changedTypedSegmentCount += updated.applied;
            return updated.text;
          }
          return part;
        });
      }
    }

    const updateSchema = (node) => {
      if (Array.isArray(node)) { for (const child of node) updateSchema(child); return; }
      if (!node || typeof node !== 'object') return;
      const agentSchema = node.name === 'Agent' ? (node.input_schema ?? node.parameters) : null;
      if (agentSchema?.properties) {
        const props = agentSchema.properties;
        if (props.send_only && JSON.stringify(props.send_only) !== JSON.stringify(sendOnly)) throw new Error(`${id}: unexpected existing send_only schema`);
        if (!props.send_only) { props.send_only = structuredClone(sendOnly); requestChanged = true; changedSchemaCount += 1; }
      }
      for (const value of Object.values(node)) updateSchema(value);
    };
    updateSchema(request);

    const systemTexts = (Array.isArray(request.system) ? request.system : [])
      .filter((part) => typeof part?.text === 'string' && part.text.includes('# Skills'))
      .map((part) => part.text);
    for (const text of systemTexts) {
      if (!text.includes('## Active modes and recovery') || !text.includes(skillsPrompt)) {
        throw new Error(`${id}/${entry.request.file}: system prompt contains stale/incomplete Skills section`);
      }
    }
    for (const rule of entry.requestRules ?? []) {
      if (rule.mode !== 'typed-template' || !Array.isArray(rule.segments)) continue;
      const rendered = rule.segments.map((part) => {
        if (typeof part === 'string') return part;
        const key = JSON.stringify([rule.owner ?? 'world', part.kind, part.alias]);
        if (!recordedBindings.has(key)) throw new Error(`${id}: typed template references an undeclared binding`);
        return recordedBindings.get(key);
      }).join('');
      const actual = valueAtPointer(request, rule.path);
      if (rendered !== actual) throw new Error(`${id}: typed template does not mirror its request field`);
      if (typeof actual === 'string' && actual.includes('# Skills')
          && (!actual.includes('## Active modes and recovery') || !actual.includes(skillsPrompt))) {
        throw new Error(`${id}: typed template contains stale/incomplete Skills section`);
      }
    }

    if (id === 'U06' && entry.identity.execution === 'manual-compact' && entry.identity.call === 'compact-command:0' && entry.identity.purpose === 'compact') {
      const system = request.system;
      if (!Array.isArray(system) || system.length !== 1 || typeof system[0].text !== 'string') throw new Error('U06 compact system prompt shape changed');
      const messages = request.messages;
      const message = messages?.at(-1);
      const content = message?.content?.find((part) => part.type === 'text' && typeof part.text === 'string');
      if (!content) throw new Error('U06 compact user instruction missing');
      const boundaryAt = content.text.indexOf(sourceIndexBoundary);
      if (boundaryAt >= 0 && content.text.indexOf(sourceIndexBoundary, boundaryAt + 1) >= 0) throw new Error('U06 duplicate source index boundary');
      const match = content.text.match(/Target at most (\d+) output tokens/);
      if (!match || !summaryUserTemplate.includes('{summary_target_tokens}')) throw new Error('U06 compact target token placeholder missing');
      const nextUser = `${summaryUserTemplate.replace('{summary_target_tokens}', match[1])}${boundaryAt >= 0 ? content.text.slice(boundaryAt) : ''}`;
      if (system[0].text !== summarySystem || content.text !== nextUser) requestChanged = true;
      system[0].text = summarySystem;
      content.text = nextUser;
    }

    if (!requestChanged) continue;
    const nextBytes = Buffer.from(JSON.stringify(request));
    stagedWrites.push({ file: requestPath, bytes: nextBytes });
    entry.request.size = nextBytes.length;
    entry.request.sha256 = sha256(nextBytes);
    const owner = entry.requestRules?.find((rule) => rule.owner)?.owner ?? 'world';
    const normalization = entry.requestRules?.length ? { bindings: registry, owner, rules: entry.requestRules } : {};
    entry.request.canonicalBody = canonicalBody(nextBytes, normalization);
    verifiedRequestBytes.push({ id, entry, bytes: nextBytes });
    cassetteChanged = true;
    changedRequestCount += 1;
  }
  if (!cassetteChanged) continue;

  const manifestBytes = Buffer.from(`${JSON.stringify(cassette.manifest, null, 2)}\n`);
  stagedWrites.push({ file: path.join(cassetteDir, 'manifest.json'), bytes: manifestBytes });
  stagedWrites.push({ file: path.join(cassetteDir, 'seal.json'), bytes: Buffer.from(`${JSON.stringify({ formatVersion: cassette.manifest.formatVersion, manifestSha256: sha256(manifestBytes) })}\n`) });
  expectedResponses.set(id, responseHashes);
  touchedCassetteCount += 1;
}

// Prepare every cassette before touching disk; malformed source or fixture data leaves the set untouched.
for (const staged of stagedWrites) await writeFile(staged.file, staged.bytes);

for (const id of cassetteIds) {
  const verified = await loadCassette(path.join(fixtures, id, 'cassette'));
  const expected = expectedResponses.get(id);
  if (expected && verified.manifest.entries.length !== expected.size) throw new Error(`${id}: entry count changed`);
  for (const entry of verified.manifest.entries) {
    if (expected && expected.get(entry.response.file) !== entry.response.sha256) throw new Error(`${id}: response manifest hash changed`);
    if (expected) responseHashCount += 1;
  }
}

// Verify each changed request against its freshly sealed expectation using exact fixture bindings.
for (const { id, entry, bytes } of verifiedRequestBytes) {
  const scenario = JSON.parse(await readFile(path.join(fixtures, id, 'scenario.json'), 'utf8'));
  const registry = createBindingRegistry({ runId: `reseal-${id}`, declarations: scenario.bindings ?? [] });
  for (const binding of scenario.bindings ?? []) registry.bind({ ...binding, live: binding.recorded });
  const owner = entry.requestRules?.find((rule) => rule.owner)?.owner ?? 'world';
  const normalization = entry.requestRules?.length ? { bindings: registry, owner, rules: entry.requestRules } : {};
  const actual = { method: entry.request.method, target: entry.request.target, headers: entry.request.headers, body: bytes };
  assertRequestMatches(entry.request, actual, normalization);
}

console.log(JSON.stringify({ cassetteCount: cassetteIds.length, touchedCassetteCount, changedRequestCount,
  changedPromptStringCount, changedTypedSegmentCount, changedSchemaCount, responseHashesChecked: responseHashCount,
  directRequestAssertions: verifiedRequestBytes.length }, null, 2));
