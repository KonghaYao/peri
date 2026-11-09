import { lstat, mkdir, open, readFile, rename, rm } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import path from 'node:path';
import { canonicalBody, canonicalJson, ReplayError, sha256 } from './matcher.mjs';
import { createBindingRegistry } from './bindings.mjs';
import { templateFor } from './template-text.mjs';

export const WORLD_FORMAT_VERSION = 2;
export const CASSETTE_FORMAT_VERSION = 2;

const STUB_VERSION = 2;
const MAX_RECIPE_DEPTH = 32;
const PART_AS = new Set(['raw', 'json-string']);

const fail = (code, message, details = {}) => { throw new ReplayError(code, message, details); };
const toBuffer = (value) => (Buffer.isBuffer(value) ? value : Buffer.from(value));
const isPlainObject = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);

async function writeDurable(file, content) {
  const handle = await open(file, 'wx', 0o600);
  try { await handle.writeFile(content); await handle.sync(); } finally { await handle.close(); }
}

function validId(value, field = 'id') {
  if (typeof value !== 'string' || !value || value.length > 512 || /[\u0000-\u001f]/u.test(value)) {
    fail('invalid_world_manifest', `${field} 必须为非空标识`);
  }
  return value;
}

/** 逻辑 leaf id 决定稳定文件路径；内容寻址 id 也走同一规则，便于 diff 只落在对应段。 */
export function worldLeafFile(id) {
  const match = /^(sys|tools|content)\.(.+)$/u.exec(validId(id));
  if (!match) fail('invalid_world_manifest', `不支持的 world leaf id: ${id}`, { leaf: id });
  const [, section, rest] = match;
  const safe = rest.replace(/[^A-Za-z0-9._-]/gu, '_');
  if (!safe || safe.startsWith('.') || safe.includes('..')) fail('invalid_world_manifest', `world leaf 路径不安全: ${id}`, { leaf: id });
  const directory = section === 'sys' ? 'system' : section;
  const extension = section === 'tools' ? 'json' : section === 'sys' ? 'txt' : 'bin';
  return `${directory}/${safe}.${extension}`;
}

function relativeFile(file, root) {
  if (typeof file !== 'string' || !file || file.includes('\0') || file.includes('\\') || path.posix.isAbsolute(file) || path.isAbsolute(file)) {
    fail('invalid_world_manifest', 'world leaf file 必须为相对路径', { file });
  }
  const normalized = path.posix.normalize(file);
  if (normalized !== file || normalized === '..' || normalized.startsWith('../')) {
    fail('invalid_world_manifest', 'world leaf file 不能越出 world 根目录', { file });
  }
  const resolved = path.resolve(root, file);
  if (resolved !== root && !resolved.startsWith(`${root}${path.sep}`)) {
    fail('invalid_world_manifest', 'world leaf file 不能越出 world 根目录', { file });
  }
  return resolved;
}

function validatePart(part, { recipeId, allowAs = false } = {}) {
  if (typeof part === 'string') {
    if (!part) fail('invalid_world_recipe', 'recipe 字面量不能为空', { recipe: recipeId });
    return;
  }
  if (!isPlainObject(part)) fail('invalid_world_recipe', 'recipe part 必须是字面量或引用', { recipe: recipeId });
  const keys = Object.keys(part);
  const refs = keys.filter((key) => ['leaf', 'recipe', 'raw'].includes(key));
  if (refs.length !== 1) fail('invalid_world_part', 'part 必须且只能引用一种来源', { recipe: recipeId });
  if (!allowAs && Object.hasOwn(part, 'as')) fail('invalid_world_part', 'recipe 内部不能用 as 二次转义', { recipe: recipeId });
  if (Object.hasOwn(part, 'as') && !PART_AS.has(part.as)) fail('invalid_world_part', '未知 part 编码', { as: part.as });
  if (Object.keys(part).some((key) => !['leaf', 'recipe', 'raw', 'as'].includes(key))) {
    fail('invalid_world_part', 'part 含未知字段', { recipe: recipeId });
  }
  if (part.leaf !== undefined) validId(part.leaf, 'leaf');
  if (part.recipe !== undefined) validId(part.recipe, 'recipe');
  if (part.raw !== undefined) {
    if (typeof part.raw !== 'string' || !part.raw) fail('invalid_request_stub', 'raw part 必须指向文件');
  }
}

function validateRecipe(parts, id) {
  if (!Array.isArray(parts) || !parts.length) fail('invalid_world_recipe', 'recipe 必须为非空数组', { recipe: id });
  for (const part of parts) {
    validatePart(part, { recipeId: id });
    if (isPlainObject(part) && part.raw !== undefined) fail('invalid_world_recipe', 'recipe 不能引用 cassette legacy 文件', { recipe: id });
  }
}

function validateGraph(leaves, recipes) {
  for (const [id, parts] of recipes) {
    for (const part of parts) {
      if (part.leaf !== undefined && !leaves.has(part.leaf)) fail('unknown_world_leaf', 'recipe 引用了不存在的 leaf', { recipe: id, leaf: part.leaf });
      if (part.recipe !== undefined && !recipes.has(part.recipe)) fail('unknown_world_recipe', 'recipe 引用了不存在的 recipe', { recipe: id, missing: part.recipe });
    }
  }
  const state = new Map();
  const visit = (id, stack) => {
    if (state.get(id) === 'done') return;
    if (state.get(id) === 'visiting') fail('world_recipe_cycle', 'world recipe 存在循环引用', { recipe: id, stack });
    state.set(id, 'visiting');
    for (const part of recipes.get(id) ?? []) {
      if (part.recipe !== undefined) visit(part.recipe, [...stack, id]);
    }
    state.set(id, 'done');
  };
  for (const id of recipes.keys()) visit(id, []);
}

function encodeAs(bytes, as = 'raw') {
  if (!PART_AS.has(as)) fail('invalid_world_part', '未知 part 编码', { as });
  if (as === 'raw') return bytes;
  let text;
  try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
  catch { fail('invalid_world_leaf', 'json-string 编码要求 UTF-8 文本'); }
  return Buffer.from(JSON.stringify(text), 'utf8');
}

function createRenderer(leaves, recipes) {
  const render = (part, stack = []) => {
    if (typeof part === 'string') return Buffer.from(part, 'utf8');
    if (!isPlainObject(part)) fail('invalid_world_part', 'part 必须是字面量或引用');
    const as = part.as ?? 'raw';
    if (part.leaf !== undefined) {
      const leaf = leaves.get(part.leaf);
      if (!leaf) fail('unknown_world_leaf', '引用了不存在的 world leaf', { leaf: part.leaf });
      return encodeAs(leaf.bytes, as);
    }
    if (part.recipe !== undefined) {
      if (stack.includes(part.recipe) || stack.length >= MAX_RECIPE_DEPTH) {
        fail('world_recipe_cycle', 'world recipe 存在循环引用', { recipe: part.recipe });
      }
      const parts = recipes.get(part.recipe);
      if (!parts) fail('unknown_world_recipe', '引用了不存在的 world recipe', { recipe: part.recipe });
      const bytes = Buffer.concat(parts.map((child) => render(child, [...stack, part.recipe])));
      return encodeAs(bytes, as);
    }
    fail('invalid_world_part', 'world 渲染只接受 leaf 或 recipe 引用');
  };
  return { render, renderParts: (parts) => Buffer.concat(parts.map((part) => render(part))) };
}

/** 迁移与录制使用：内存中构建 world，seal 后一次性落到目录。 */
export function createWorldBuilder({ generation } = {}) {
  const leaves = new Map();
  const recipes = new Map();
  const renderer = createRenderer(leaves, recipes);

  const addLeaf = (id, content) => {
    validId(id);
    const bytes = toBuffer(content);
    const existing = leaves.get(id);
    if (existing) {
      if (!existing.bytes.equals(bytes)) fail('duplicate_world_leaf', '同一 leaf id 不能对应不同内容', { leaf: id });
      return id;
    }
    leaves.set(id, { file: worldLeafFile(id), bytes });
    return id;
  };

  return {
    addLeaf,
    addContentLeaf(section, content) {
      const prefix = section === 'system' || section === 'sys' ? 'sys' : section === 'tools' ? 'tools' : null;
      if (!prefix) fail('invalid_world_manifest', '内容寻址只支持 system 或 tools 段', { section });
      const bytes = toBuffer(content);
      return addLeaf(`${prefix}.sha256.${sha256(bytes)}`, bytes);
    },
    addRecipe(id, parts) {
      validId(id);
      if (recipes.has(id) || leaves.has(id)) fail('duplicate_world_recipe', 'world id 已存在', { recipe: id });
      const copy = structuredClone(parts);
      validateRecipe(copy, id);
      recipes.set(id, copy);
      return id;
    },
    hasLeaf: (id) => leaves.has(id),
    leafBytes: (id) => leaves.get(id)?.bytes,
    render: renderer.render,
    renderParts: renderer.renderParts,
    async seal(directory) {
      validateGraph(leaves, recipes);
      const requested = path.resolve(directory);
      await mkdir(path.dirname(requested), { recursive: true });
      try { await lstat(requested); fail('world_exists', 'world 目标已存在，禁止覆盖'); }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      const sortedLeaves = [...leaves.entries()].sort(([left], [right]) => left.localeCompare(right));
      const sortedRecipes = [...recipes.entries()].sort(([left], [right]) => left.localeCompare(right));
      const files = new Set();
      for (const [id, leaf] of sortedLeaves) {
        if (files.has(leaf.file)) fail('duplicate_world_leaf', '不同 leaf id 不能映射到同一文件', { leaf: id, file: leaf.file });
        files.add(leaf.file);
      }
      // generation 只跟随 slot/recipe 结构，不跟随 leaf 内容；否则改一段 prompt 会连带改动 world manifest。
      const structureDigest = sha256(canonicalJson({
        leaves: sortedLeaves.map(([id]) => id),
        recipes: Object.fromEntries(sortedRecipes),
      }));
      const manifest = {
        formatVersion: WORLD_FORMAT_VERSION,
        kind: 'world',
        generation: generation ?? `world-${structureDigest.slice(0, 16)}`,
        leaves: Object.fromEntries(sortedLeaves.map(([id, leaf]) => [id, { file: leaf.file }])),
        recipes: Object.fromEntries(sortedRecipes),
      };
      const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
      const seal = {
        formatVersion: WORLD_FORMAT_VERSION,
        manifestSha256: sha256(manifestBytes),
        leaves: Object.fromEntries(sortedLeaves.map(([id, leaf]) => [id, sha256(leaf.bytes)])),
      };
      const staging = `${requested}.partial-${randomUUID()}`;
      await mkdir(staging, { mode: 0o700 });
      try {
        for (const [, leaf] of sortedLeaves) {
          const file = relativeFile(leaf.file, staging);
          await mkdir(path.dirname(file), { recursive: true, mode: 0o700 });
          await writeDurable(file, leaf.bytes);
        }
        await writeDurable(path.join(staging, 'manifest.json'), manifestBytes);
        await writeDurable(path.join(staging, 'seal.json'), `${JSON.stringify(seal)}\n`);
        const handle = await open(staging, 'r');
        try { await handle.sync(); } finally { await handle.close(); }
        await rename(staging, requested);
      } catch (error) {
        await rm(staging, { recursive: true, force: true });
        throw error;
      }
      return { directory: requested, generation: manifest.generation, manifestSha256: seal.manifestSha256, leaves: leaves.size, recipes: recipes.size };
    },
  };
}

/** 读取并校验 world seal；每个 leaf 按 manifest 声明与 seal 哈希逐字节核对。 */
export async function loadWorld(directory) {
  const root = path.resolve(directory);
  let manifestBytes;
  let sealBytes;
  try { manifestBytes = await readFile(path.join(root, 'manifest.json')); }
  catch (error) { fail('unsealed_world', 'world manifest 缺失或无法读取', { reason: String(error) }); }
  try { sealBytes = await readFile(path.join(root, 'seal.json')); }
  catch (error) { fail('unsealed_world', 'world seal 缺失或无法读取', { reason: String(error) }); }
  let seal;
  try { seal = JSON.parse(sealBytes.toString('utf8')); }
  catch { fail('corrupt_world', 'world seal 不是合法 JSON'); }
  if (!isPlainObject(seal) || seal.formatVersion !== WORLD_FORMAT_VERSION || seal.manifestSha256 !== sha256(manifestBytes)) {
    fail('corrupt_world', 'world manifest 校验失败');
  }
  let manifest;
  try { manifest = JSON.parse(manifestBytes.toString('utf8')); }
  catch { fail('corrupt_world', 'world manifest 不是合法 JSON'); }
  if (!isPlainObject(manifest) || manifest.formatVersion !== WORLD_FORMAT_VERSION || manifest.kind !== 'world'
      || typeof manifest.generation !== 'string' || !manifest.generation
      || !isPlainObject(manifest.leaves) || !isPlainObject(manifest.recipes) || !isPlainObject(seal.leaves)) {
    fail('corrupt_world', 'world manifest 结构无效');
  }
  const manifestIds = Object.keys(manifest.leaves).sort();
  const sealIds = Object.keys(seal.leaves).sort();
  if (manifestIds.length !== sealIds.length || manifestIds.some((id, index) => id !== sealIds[index])) {
    const missing = manifestIds.find((id) => !Object.hasOwn(seal.leaves, id));
    const extra = sealIds.find((id) => !Object.hasOwn(manifest.leaves, id));
    fail('corrupt_world', 'world seal 未覆盖全部 leaf', { leaf: missing ?? extra ?? null });
  }
  const leaves = new Map();
  for (const [id, meta] of Object.entries(manifest.leaves)) {
    validId(id, 'leaf');
    if (!isPlainObject(meta) || Object.keys(meta).some((key) => key !== 'file')) fail('corrupt_world', 'world leaf 声明无效', { leaf: id });
    const file = relativeFile(meta.file, root);
    let stat;
    try { stat = await lstat(file); }
    catch (error) {
      fail('corrupt_world', 'world leaf 缺失', { leaf: id, reason: String(error) });
    }
    if (!stat.isFile() || stat.isSymbolicLink()) fail('corrupt_world', 'world leaf 必须为普通文件', { leaf: id });
    const bytes = await readFile(file);
    if (seal.leaves[id] !== sha256(bytes)) fail('corrupt_world', 'world leaf 校验失败', { leaf: id });
    leaves.set(id, { file: meta.file, bytes });
  }
  const recipes = new Map();
  for (const [id, parts] of Object.entries(manifest.recipes)) {
    validId(id, 'recipe');
    validateRecipe(parts, id);
    recipes.set(id, parts);
  }
  validateGraph(leaves, recipes);
  const renderer = createRenderer(leaves, recipes);
  return Object.freeze({
    root, manifest, seal,
    leaves: [...leaves.keys()],
    recipes: [...recipes.keys()],
    hasLeaf: (id) => leaves.has(id),
    leafBytes: (id) => leaves.get(id)?.bytes,
    render: renderer.render,
    renderParts: renderer.renderParts,
  });
}

function parseSpanJson(text) {
  let pos = 0;
  let unsafeNumber = false;
  const failSpan = (code = 'invalid_json') => fail(code, `JSON 无效，偏移 ${pos}`);
  const space = () => { while (pos < text.length && /[\x20\t\r\n]/u.test(text[pos])) pos += 1; };
  const string = () => {
    const start = pos;
    pos += 1;
    while (pos < text.length) {
      const char = text[pos];
      pos += 1;
      if (char === '\\') pos += 1;
      else if (char === '"') {
        try { return { type: 'string', start, end: pos, value: JSON.parse(text.slice(start, pos)) }; }
        catch { failSpan(); }
      }
    }
    return failSpan();
  };
  const number = () => {
    const start = pos;
    const match = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/u.exec(text.slice(pos));
    if (!match) failSpan();
    pos += match[0].length;
    const numeric = Number(match[0]);
    if (!Number.isFinite(numeric) || (/^-?(?:0|[1-9][0-9]*)$/u.test(match[0]) && !Number.isSafeInteger(numeric))) unsafeNumber = true;
    return { type: 'number', start, end: pos, raw: match[0] };
  };
  const value = (depth = 0) => {
    if (depth > 256) failSpan();
    space();
    if (text[pos] === '"') return string();
    if (text[pos] === '{') {
      const start = pos++;
      const entries = [];
      const keys = new Set();
      space();
      if (text[pos] === '}') { pos += 1; return { type: 'object', start, end: pos, entries }; }
      while (pos < text.length) {
        space();
        if (text[pos] !== '"') failSpan();
        const key = string();
        if (keys.has(key.value)) failSpan('duplicate_json_key');
        keys.add(key.value);
        space();
        if (text[pos++] !== ':') failSpan();
        entries.push({ key: key.value, value: value(depth + 1) });
        space();
        if (text[pos] === '}') { pos += 1; return { type: 'object', start, end: pos, entries }; }
        if (text[pos++] !== ',') failSpan();
      }
      failSpan();
    }
    if (text[pos] === '[') {
      const start = pos++;
      const items = [];
      space();
      if (text[pos] === ']') { pos += 1; return { type: 'array', start, end: pos, items }; }
      while (pos < text.length) {
        items.push(value(depth + 1));
        space();
        if (text[pos] === ']') { pos += 1; return { type: 'array', start, end: pos, items }; }
        if (text[pos++] !== ',') failSpan();
      }
      failSpan();
    }
    for (const [token, result] of [['true', true], ['false', false], ['null', null]]) {
      if (text.startsWith(token, pos)) { const start = pos; pos += token.length; return { type: typeof result === 'boolean' ? 'boolean' : 'null', start, end: pos, value: result }; }
    }
    const parsed = number();
    return parsed;
  };
  const root = value();
  space();
  if (pos !== text.length) failSpan();
  root.unsafeNumber = unsafeNumber;
  return root;
}

/** 解析带字节跨度的严格 JSON；请求 body 的重建只 splice 原字节，不重新序列化上下文字段。 */
export function parseRequestSpans(text) {
  return parseSpanJson(text);
}

const member = (node, key) => {
  if (node?.type !== 'object') return undefined;
  for (let index = node.entries.length - 1; index >= 0; index -= 1) {
    if (node.entries[index].key === key) return node.entries[index].value;
  }
  return undefined;
};

function collectWorldFields(root) {
  if (root.type !== 'object') return [];
  const fields = [];
  const system = member(root, 'system');
  if (system?.type === 'string') {
    fields.push({ kind: 'system', path: '/system', text: system.value, start: system.start, end: system.end, as: 'json-string' });
  } else if (system?.type === 'array') {
    for (const [index, item] of system.items.entries()) {
      const text = member(item, 'text');
      if (text?.type === 'string') fields.push({ kind: 'system', path: `/system/${index}/text`, text: text.value, start: text.start, end: text.end, as: 'json-string' });
    }
  }
  const instructions = member(root, 'instructions');
  if (instructions?.type === 'string') fields.push({ kind: 'instructions', path: '/instructions', text: instructions.value, start: instructions.start, end: instructions.end, as: 'json-string' });
  const input = member(root, 'input');
  if (input?.type === 'array') {
    for (const [index, item] of input.items.entries()) {
      const role = member(item, 'role');
      if (role?.type !== 'string' || !['developer', 'system'].includes(role.value)) continue;
      const content = member(item, 'content');
      if (content?.type === 'string') {
        fields.push({ kind: 'responses', path: `/input/${index}/content`, text: content.value, start: content.start, end: content.end, as: 'json-string' });
      } else if (content?.type === 'array') {
        for (const [part, block] of content.items.entries()) {
          const text = member(block, 'text');
          if (text?.type === 'string') fields.push({ kind: 'responses', path: `/input/${index}/content/${part}/text`, text: text.value, start: text.start, end: text.end, as: 'json-string' });
        }
      }
    }
  }
  const tools = member(root, 'tools');
  if (tools) {
    fields.push({ kind: 'tools', path: '/tools', start: tools.start, end: tools.end, as: 'raw' });
  }
  fields.sort((left, right) => left.start - right.start);
  return fields;
}

/**
 * world 字段按原字节 splice 成 stub；无法稳定重建时整条请求退化为 legacy body。
 * 调用方必须保证 slots 返回的引用在目标 world 中可渲染。
 */
export function encodeRequestStub(rawBytes, { world, slots } = {}) {
  const bytes = toBuffer(rawBytes);
  const base = { v: STUB_VERSION, rawBytes: bytes.length, rawSha256: sha256(bytes) };
  let text;
  try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
  catch { return { ...base, legacy: true, reason: 'invalid_utf8' }; }
  let root;
  try { root = parseSpanJson(text); }
  catch (error) {
    if (error instanceof ReplayError) return { ...base, legacy: true, reason: error.code };
    throw error;
  }
  if (root.unsafeNumber) return { ...base, legacy: true, reason: 'unsafe_number' };
  if (!world && !slots) return { ...base, parts: [text] };
  if (slots && (!world || typeof world.renderParts !== 'function')) fail('invalid_world_writer', 'slots 必须搭配可渲染的 world 以校验字节重建');
  const fields = collectWorldFields(root);
  if (!fields.length) return { ...base, parts: [text] };
  const parts = [];
  let cursor = 0;
  for (const field of fields) {
    if (field.start < cursor) return { ...base, legacy: true, reason: 'unstable_split' };
    if (field.start > cursor) parts.push(text.slice(cursor, field.start));
    let part = slots ? slots(field) : null;
    if (part === undefined) part = null;
    if (part === null) {
      if (!world || typeof world.addContentLeaf !== 'function') fail('invalid_world_writer', '缺少可写入的 world，无法生成内容寻址快照');
      if (field.kind === 'tools') {
        part = { leaf: world.addContentLeaf('tools', Buffer.from(text.slice(field.start, field.end), 'utf8')), as: 'raw' };
      } else {
        if (JSON.stringify(field.text) !== text.slice(field.start, field.end)) return { ...base, legacy: true, reason: 'text_encoding_mismatch' };
        part = { leaf: world.addContentLeaf('system', Buffer.from(field.text, 'utf8')), as: 'json-string' };
      }
    } else {
      validatePart(part, { allowAs: true });
      if (part.raw !== undefined) fail('invalid_world_part', 'world 字段不能引用 raw 文件');
      const expectedAs = field.kind === 'tools' ? 'raw' : 'json-string';
      part = { ...part, as: part.as ?? expectedAs };
      if (part.as !== expectedAs) fail('invalid_world_part', 'world 字段编码与字段类型不符', { path: field.path });
    }
    parts.push(part);
    cursor = field.end;
  }
  if (cursor < text.length) parts.push(text.slice(cursor));
  if (world && typeof world.renderParts === 'function') {
    const rebuilt = world.renderParts(parts);
    if (!rebuilt.equals(bytes)) fail('invalid_request_stub', 'world 字段拼接后与原始 body 不一致');
  }
  return { ...base, parts };
}

export function serializeRequestStub(stub) {
  return Buffer.from(`${JSON.stringify(stub, null, 2)}\n`);
}

function validateStubHeader(stub) {
  if (!isPlainObject(stub) || stub.v !== STUB_VERSION) fail('invalid_request_stub', '不支持的 request stub 版本');
  if (!Array.isArray(stub.parts) || !stub.parts.length) fail('invalid_request_stub', 'request stub 缺少 parts');
  if (typeof stub.rawSha256 !== 'string' || !/^[0-9a-f]{64}$/u.test(stub.rawSha256)) fail('invalid_request_stub', 'request stub 缺少 rawSha256');
  if (stub.rawBytes !== undefined && (!Number.isSafeInteger(stub.rawBytes) || stub.rawBytes < 0)) fail('invalid_request_stub', 'request stub rawBytes 无效');
  if (stub.canonicalSha256 !== undefined && (typeof stub.canonicalSha256 !== 'string' || !/^[0-9a-f]{64}$/u.test(stub.canonicalSha256))) {
    fail('invalid_request_stub', 'request stub canonicalSha256 无效');
  }
}

function verifyRaw(stub, bytes) {
  if (stub.rawBytes !== undefined && bytes.length !== stub.rawBytes) fail('corrupt_cassette', 'request body 长度不符', { rawSha256: stub.rawSha256 });
  if (sha256(bytes) !== stub.rawSha256) fail('corrupt_cassette', 'request body 校验失败', { rawSha256: stub.rawSha256 });
}

/** 由 stub 重建 raw body；legacy part 必须由调用方提供受 seal 覆盖的读取器。 */
export async function decodeRequestStub(stub, { world, readRaw } = {}) {
  validateStubHeader(stub);
  const rawParts = stub.parts.filter((part) => isPlainObject(part) && Object.hasOwn(part, 'raw'));
  if (rawParts.length) {
    if (stub.parts.length !== 1) fail('invalid_request_stub', 'legacy body 不能与其它 parts 混用');
    if (typeof rawParts[0].raw !== 'string' || !rawParts[0].raw) fail('invalid_request_stub', 'legacy part 必须指向文件');
    if (!readRaw) fail('invalid_request_stub', '缺少 legacy body 读取器');
    const bytes = await readRaw(rawParts[0].raw);
    verifyRaw(stub, bytes);
    return { bytes, legacy: true, legacyFile: rawParts[0].raw };
  }
  const chunks = stub.parts.map((part) => {
    if (typeof part === 'string') return Buffer.from(part, 'utf8');
    if (!world) fail('missing_world', 'request stub 引用了 world，但未提供 world');
    validatePart(part, { allowAs: true });
    if (part.raw !== undefined) fail('invalid_request_stub', 'legacy body 必须独占 parts');
    return world.render(part);
  });
  const bytes = Buffer.concat(chunks);
  verifyRaw(stub, bytes);
  return { bytes, legacy: false };
}

const recordedKey = (kind, alias) => `${kind}:${alias}`;
const patternDigest = (pattern) => sha256(canonicalJson(pattern));
const pointerTokens = (pointer) => {
  if (typeof pointer !== 'string' || !pointer.startsWith('/')) fail('invalid_request_stub', '绑定路径必须为非根 JSON Pointer');
  return pointer.slice(1).split('/').map((token) => token.replace(/~1/gu, '/').replace(/~0/gu, '~'));
};
const valueAtPointer = (root, pointer) => pointerTokens(pointer).reduce((value, token) => value?.[token], root);

function renderRecordedSegments(segments, values) {
  return segments.map((part) => {
    if (typeof part === 'string') return part;
    const value = values[recordedKey(part.kind, part.alias)];
    if (typeof value !== 'string') fail('invalid_binding_template', '模板引用了未记录的绑定值', { kind: part.kind, alias: part.alias });
    return value;
  }).join('');
}

/** 录制时把 segments 压缩为 pattern + live/recorded；同时断言实际 body 与 live 渲染一致。 */
export function compactTypedTemplateRules(rules, { bindings, owner = 'world', bodyBytes } = {}) {
  if (!Array.isArray(rules)) fail('invalid_binding_rules', 'rules 必须是数组');
  if (!rules.length) return [];
  if (!bindings || typeof bindings.snapshot !== 'function') fail('missing_bindings', 'request 声明了绑定规则，但未提供录制绑定表');
  const records = new Map(bindings.snapshot().map((record) => [JSON.stringify([record.owner, record.kind, record.alias]), record]));
  const body = bodyBytes === undefined ? null : JSON.parse(toBuffer(bodyBytes).toString('utf8'));
  return rules.map((rule) => {
    if (rule.mode !== 'typed-template') {
      if (!['exact', 'path-prefix'].includes(rule.mode)) fail('invalid_binding_mode', '不支持的绑定模式', { mode: rule.mode });
      if (!rule.kind || !rule.alias) fail('invalid_binding_rule', '非模板规则必须声明 kind 与 alias', { path: rule.path });
      const ruleOwner = rule.owner ?? owner;
      const record = records.get(JSON.stringify([ruleOwner, rule.kind, rule.alias]));
      if (!record) fail('binding_not_declared', '规则引用了未登记的绑定', { kind: rule.kind, alias: rule.alias });
      if (body) {
        const actual = valueAtPointer(body, rule.path);
        const ok = rule.mode === 'path-prefix'
          ? typeof actual === 'string' && (actual === record.live || actual.startsWith(`${record.live}/`))
          : actual === record.live;
        if (!ok) fail('binding_value_mismatch', `绑定字段不符合其声明身份: ${rule.path}`);
      }
      return { ...rule, live: record.live, recorded: record.recorded };
    }
    if (!Array.isArray(rule.segments) || !rule.segments.length
        || !rule.segments.some((part) => part && typeof part === 'object' && !Array.isArray(part))) {
      fail('invalid_binding_template', 'typed-template 必须包含显式类型引用', { path: rule.path });
    }
    const ruleOwner = rule.owner ?? owner;
    const pattern = [];
    const recorded = {};
    const live = {};
    for (const part of rule.segments) {
      if (typeof part === 'string') continue;
      if (!isPlainObject(part) || Object.keys(part).some((key) => !['kind', 'alias'].includes(key))) {
        fail('invalid_binding_template', '模板引用仅允许 kind 与 alias', { path: rule.path });
      }
      const record = records.get(JSON.stringify([ruleOwner, part.kind, part.alias]));
      if (!record) fail('binding_not_declared', '模板引用了未登记的绑定', { kind: part.kind, alias: part.alias });
      const key = recordedKey(part.kind, part.alias);
      if (recorded[key] !== undefined && recorded[key] !== record.recorded) fail('duplicate_binding_alias', '同一模板引用对应多个录制值', { path: rule.path });
      recorded[key] = record.recorded;
      live[key] = record.live;
      pattern.push({ kind: part.kind, alias: part.alias });
    }
    if (body) {
      const actual = valueAtPointer(body, rule.path);
      if (typeof actual !== 'string' || renderRecordedSegments(rule.segments, live) !== actual) {
        fail('binding_value_mismatch', `typed-template 与请求字段不一致: ${rule.path}`);
      }
    }
    const compact = { ...rule, pattern, patternSha256: patternDigest(pattern), recorded, live };
    delete compact.segments;
    return compact;
  });
}

function systemTextFor(rule, body) {
  if (typeof rule.systemText === 'boolean') return rule.systemText;
  const tokens = pointerTokens(rule.path);
  if (tokens[0] === 'system' || tokens[0] === 'instructions') return true;
  if (tokens[0] === 'input' && tokens[1] !== undefined) {
    const item = body.input?.[Number(tokens[1])];
    return Boolean(item && ['developer', 'system'].includes(item.role));
  }
  return false;
}

function segmentsFromPattern(text, pattern, recorded, systemText, rulePath) {
  const distinct = new Map();
  for (const reference of pattern) {
    const key = recordedKey(reference.kind, reference.alias);
    const value = recorded[key];
    if (typeof value !== 'string' || !value) fail('binding_template_ambiguous', 'pattern 缺少录制值', { path: rulePath, reference: key });
    distinct.set(key, { kind: reference.kind, alias: reference.alias, recorded: value });
  }
  // 复用录制时的同一套边界扫描：pattern 重复引用同一别名表示该值在字段中重复出现。
  const segments = templateFor(text, [...distinct.values()], 'recorded', systemText);
  if (!segments) fail('binding_template_ambiguous', '绑定值在字段中一次也未出现', { path: rulePath });
  const actual = segments.filter((part) => typeof part !== 'string').map(({ kind, alias }) => ({ kind, alias }));
  const same = actual.length === pattern.length
    && actual.every((reference, index) => reference.kind === pattern[index].kind && reference.alias === pattern[index].alias);
  if (!same) fail('binding_template_ambiguous', '绑定出现序列与冻结 pattern 不符', { path: rulePath, expected: pattern, actual });
  return segments;
}

/** load 时用重建文本与 live 值（缺失时退回 recorded）重新定位 pattern，展开成现有 segments 形状。 */
export function expandTypedTemplateRules(rules, rawBytes) {
  if (!Array.isArray(rules)) fail('invalid_request_stub', 'requestRules 必须是数组');
  if (!rules.length) return { rules: [], bindings: [] };
  const body = JSON.parse(toBuffer(rawBytes).toString('utf8'));
  const expanded = [];
  const bindings = [];
  for (const rule of rules) {
    if (!isPlainObject(rule) || typeof rule.path !== 'string') fail('invalid_request_stub', 'requestRules 条目无效');
    if (rule.mode !== 'typed-template') {
      if (!['exact', 'path-prefix'].includes(rule.mode)) fail('invalid_request_stub', '不支持的绑定模式', { mode: rule.mode });
      if (!rule.kind || !rule.alias || typeof rule.recorded !== 'string' || (rule.live !== undefined && typeof rule.live !== 'string')) {
        fail('invalid_request_stub', '非模板规则缺少冻结的绑定值', { path: rule.path });
      }
      expanded.push(rule);
      bindings.push({ owner: rule.owner ?? 'world', kind: rule.kind, alias: rule.alias, recorded: rule.recorded, live: rule.live ?? rule.recorded });
      continue;
    }
    if (!Array.isArray(rule.pattern) || !rule.pattern.length) fail('invalid_request_stub', 'typed-template 缺少 pattern', { path: rule.path });
    if (rule.segments !== undefined) fail('invalid_request_stub', 'typed-template 不能同时携带 pattern 与 segments', { path: rule.path });
    if (!isPlainObject(rule.recorded)) fail('invalid_request_stub', 'typed-template 缺少 recorded', { path: rule.path });
    if (rule.live !== undefined && !isPlainObject(rule.live)) fail('invalid_request_stub', 'typed-template live 映射无效', { path: rule.path });
    if (rule.patternSha256 !== patternDigest(rule.pattern)) fail('binding_template_pattern_mismatch', 'typed-template pattern 校验失败', { path: rule.path });
    // 录制时 live 可能与 recorded 不同；定位始终用实际 body 中的 live 值。
    const source = isPlainObject(rule.live) && Object.keys(rule.live).length ? rule.live : rule.recorded;
    const recordedKeys = new Set();
    for (const reference of rule.pattern) {
      if (!isPlainObject(reference) || Object.keys(reference).some((key) => !['kind', 'alias'].includes(key))
          || typeof reference.kind !== 'string' || typeof reference.alias !== 'string') {
        fail('invalid_request_stub', 'typed-template pattern 引用无效', { path: rule.path });
      }
      const key = recordedKey(reference.kind, reference.alias);
      recordedKeys.add(key);
      if (typeof rule.recorded[key] !== 'string') fail('invalid_request_stub', 'pattern 缺少 recorded 值', { path: rule.path, reference: key });
      if (typeof source[key] !== 'string') fail('invalid_request_stub', 'pattern 缺少 live 值', { path: rule.path, reference: key });
      bindings.push({ owner: rule.owner ?? 'world', kind: reference.kind, alias: reference.alias, recorded: rule.recorded[key], live: source[key] });
    }
    for (const key of Object.keys(rule.recorded)) {
      if (!recordedKeys.has(key)) fail('binding_template_ambiguous', 'recorded 含 pattern 未声明的值', { path: rule.path, reference: key });
    }
    for (const key of Object.keys(source)) {
      if (!recordedKeys.has(key)) fail('binding_template_ambiguous', 'live 含 pattern 未声明的值', { path: rule.path, reference: key });
    }
    const text = valueAtPointer(body, rule.path);
    if (typeof text !== 'string') fail('invalid_request_stub', 'typed-template 目标字段必须为字符串', { path: rule.path });
    const segments = segmentsFromPattern(text, rule.pattern, source, systemTextFor(rule, body), rule.path);
    expanded.push({
      ...(rule.owner !== undefined ? { owner: rule.owner } : {}),
      path: rule.path,
      mode: 'typed-template',
      segments,
    });
  }
  return { rules: expanded, bindings };
}

/** 用 live 值定位、recorded 侧重建 canonical body；写入时已断言两者与原始字段一致。 */
export function canonicalRecordedBody(rawBytes, rules, bindingList) {
  if (!rules.length) return canonicalBody(rawBytes);
  const owners = new Set(rules.map((rule) => rule.owner ?? 'world'));
  if (owners.size !== 1) fail('corrupt_cassette', 'cassette 规则不能跨 owner', { owners: [...owners] });
  const owner = [...owners][0];
  const unique = new Map();
  for (const binding of bindingList) {
    const key = JSON.stringify([binding.owner, binding.kind, binding.alias]);
    const existing = unique.get(key);
    if (existing && (existing.recorded !== binding.recorded || existing.live !== binding.live)) {
      fail('corrupt_cassette', 'cassette 同一别名对应多个绑定值', { kind: binding.kind, alias: binding.alias });
    }
    unique.set(key, binding);
  }
  const declarations = [...unique.values()].map((binding) => ({
    owner: binding.owner, kind: binding.kind, alias: binding.alias, recorded: binding.recorded, anchor: 'cassette-load',
  }));
  const registry = createBindingRegistry({ runId: 'cassette-load', declarations });
  for (const declaration of declarations) {
    const binding = unique.get(JSON.stringify([declaration.owner, declaration.kind, declaration.alias]));
    registry.bind({ ...declaration, live: binding.live ?? binding.recorded });
  }
  return canonicalBody(rawBytes, { bindings: registry, owner, rules });
}
