import { posix as path } from 'node:path';
import { BindingError } from './binding-error.mjs';

export { BindingError } from './binding-error.mjs';

const KINDS = new Set(['session', 'thread', 'task', 'execution', 'tool_call', 'path', 'date']);
const PROTECTED_KEYS = new Set(['signature', 'encrypted_content', 'provider_state', 'providerState', 'reasoning_content', 'thoughtSignature', 'thought_signature']);
const PROTECTED_TYPES = new Set(['thinking', 'redacted_thinking', 'reasoning', 'redacted_reasoning', 'provider_state']);

function fail(code, message) {
  throw new BindingError(code, message);
}

function name(value, field) {
  if (typeof value !== 'string' || value.length === 0 || /[\u0000-\u001f]/u.test(value)) {
    fail('invalid_binding', `${field} 必须是非空且不含控制字符的字符串`);
  }
  return value;
}

function keyOf({ owner, kind, alias }) {
  name(owner, 'owner');
  name(alias, 'alias');
  if (!KINDS.has(kind)) fail('invalid_binding_kind', `不支持的绑定类型: ${kind}`);
  return JSON.stringify([owner, kind, alias]);
}

function bindingValue(value, kind, field) {
  name(value, field);
  if (kind === 'path' && (!path.isAbsolute(value) || value === '/' || path.normalize(value) !== value)) {
    fail('invalid_binding_path', `${field} 必须是规范的绝对 POSIX 路径且不能是根目录`);
  }
  if (kind === 'date' && (!/^\d{4}-\d{2}-\d{2}$/u.test(value) ||
      Number.isNaN(Date.parse(`${value}T00:00:00Z`)) || new Date(`${value}T00:00:00Z`).toISOString().slice(0, 10) !== value)) {
    fail('invalid_binding_date', `${field} 必须是有效的 YYYY-MM-DD 日期`);
  }
  return value;
}

function valueKeyOf({ owner, kind }, value) {
  // 多个会话可共享同一工作区；身份 ID 不允许跨 owner 认领，路径别名只在 owner 内唯一。
  return JSON.stringify(['path', 'date'].includes(kind) ? [owner, kind, value] : [kind, value]);
}

function pointerTokens(pointer) {
  if (typeof pointer !== 'string' || !pointer.startsWith('/') || /~(?:[^01]|$)/u.test(pointer)) {
    fail('invalid_binding_pointer', '绑定路径必须是精确 JSON Pointer');
  }
  const tokens = pointer.slice(1).split('/').map((token) => token.replace(/~1/g, '/').replace(/~0/g, '~'));
  if (tokens.some((token) => ['__proto__', 'prototype', 'constructor', '*'].includes(token))) {
    fail('invalid_binding_pointer', '绑定路径不接受通配符或原型属性');
  }
  return tokens;
}

function protectedNode(value) {
  return value !== null && typeof value === 'object' &&
    (PROTECTED_TYPES.has(value.type) || PROTECTED_TYPES.has(value.kind));
}

function targetAt(body, tokens, pointer) {
  let parent = body;
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (protectedNode(parent) || PROTECTED_KEYS.has(token)) {
      fail('protected_binding_target', `不能归一化或改写签名及不透明状态: ${pointer}`);
    }
    if (parent === null || typeof parent !== 'object' || !Object.hasOwn(parent, token) ||
        (Array.isArray(parent) && !/^(0|[1-9]\d*)$/u.test(token))) {
      fail('binding_path_missing', `绑定路径不存在: ${pointer}`);
    }
    if (i === tokens.length - 1) return { parent, key: token };
    parent = parent[token];
  }
  fail('invalid_binding_pointer', '不接受根对象替换');
}

function cloneJson(value) {
  const visit = (node) => {
    if (node === null || typeof node === 'string' || typeof node === 'boolean') return;
    if (typeof node === 'number' && Number.isFinite(node)) return;
    if (Array.isArray(node)) {
      for (let i = 0; i < node.length; i += 1) {
        if (!Object.hasOwn(node, i)) fail('invalid_binding_body', '不接受稀疏数组');
        visit(node[i]);
      }
      return;
    }
    if (typeof node === 'object' && Object.getPrototypeOf(node) === Object.prototype) {
      for (const child of Object.values(node)) visit(child);
      return;
    }
    fail('invalid_binding_body', '绑定仅接受 JSON 数据');
  };
  visit(value);
  return structuredClone(value);
}

/** 每次 replay run 独立持有；声明来自 fixture，live 值由匹配的创建事件登记。 */
export function createBindingRegistry({ runId, declarations = [] } = {}) {
  name(runId, 'runId');
  const records = new Map();
  const recordedValues = new Map();
  const liveValues = new Map();

  function declare(input) {
    const key = keyOf(input);
    const recorded = bindingValue(input.recorded, input.kind, 'recorded');
    const anchor = name(input.anchor, 'anchor');
    if (records.has(key)) fail('duplicate_binding_alias', '绑定别名已经声明');
    const valueKey = valueKeyOf(input, recorded);
    if (recordedValues.has(valueKey)) {
      fail('binding_alias_collision', '同一录制身份不能声明为多个 owner 或别名');
    }
    const record = Object.freeze({ owner: input.owner, kind: input.kind, alias: input.alias, recorded, anchor });
    records.set(key, record);
    recordedValues.set(valueKey, key);
    return { ...record };
  }

  function bind(input) {
    const key = keyOf(input);
    const record = records.get(key);
    if (!record) fail('binding_not_declared', 'live 绑定必须先在 fixture 中声明');
    if (input.anchor !== record.anchor) fail('binding_anchor_mismatch', '创建事件与声明的因果锚点不同');
    const live = bindingValue(input.live, record.kind, 'live');
    if (record.live !== undefined) {
      if (record.live !== live) fail('binding_alias_rebound', '同一别名不能换绑到另一个实际身份');
      return { ...record };
    }
    const valueKey = valueKeyOf(record, live);
    if (liveValues.has(valueKey) && liveValues.get(valueKey) !== key) {
      fail('binding_alias_collision', '同一实际身份不能绑定到多个 owner 或别名');
    }
    const bound = Object.freeze({ ...record, live });
    records.set(key, bound);
    liveValues.set(valueKey, key);
    return { ...bound };
  }

  function transform(body, { owner, rules = [], source = 'live' } = {}, direction) {
    name(owner, 'owner');
    if (!Array.isArray(rules)) fail('invalid_binding_rules', 'rules 必须是数组');
    if (!['live', 'recorded'].includes(source)) fail('invalid_binding_source', 'source 必须是 live 或 recorded');
    const result = cloneJson(body);
    const touched = new Set();
    const resolve = (reference) => {
      const binding = records.get(keyOf({ owner, kind: reference.kind, alias: reference.alias }));
      if (!binding) fail('binding_not_declared', '规则引用了本 owner 未声明的绑定');
      if (binding.live === undefined) fail('binding_unbound', '转换前所有引用都必须完成 live 绑定');
      return binding;
    };
    for (const rule of rules) {
      if (rule.owner !== undefined && rule.owner !== owner) {
        fail('binding_owner_mismatch', '规则不能跨 owner 自动查找绑定');
      }
      const tokens = pointerTokens(rule.path);
      const canonicalPointer = JSON.stringify(tokens);
      if (touched.has(canonicalPointer)) fail('duplicate_binding_rule', '同一 JSON 路径只能声明一条绑定规则');
      touched.add(canonicalPointer);
      const { parent, key: property } = targetAt(result, tokens, rule.path);
      const mode = rule.mode ?? 'exact';
      if (mode === 'typed-template') {
        if (!Array.isArray(rule.segments) || !rule.segments.length || !rule.segments.some((part) => typeof part !== 'string')) {
          fail('invalid_binding_template', '模板必须包含显式类型引用和原样文字');
        }
        const render = (side) => rule.segments.map((part) => {
          if (typeof part === 'string') return part;
          if (!part || typeof part !== 'object' || Array.isArray(part) ||
              Object.keys(part).some((key) => !['kind', 'alias'].includes(key))) {
            fail('invalid_binding_template', '模板引用仅允许 kind 与 alias');
          }
          return resolve(part)[side];
        }).join('');
        const expected = render(direction === 'response' || source === 'recorded' ? 'recorded' : 'live');
        if (parent[property] !== expected) fail('binding_value_mismatch', `模板文字或绑定值与声明不符: ${rule.path}`);
        parent[property] = render(direction === 'response' ? 'live' : 'recorded');
        continue;
      }
      const binding = resolve(rule);
      const from = direction === 'response' || source === 'recorded' ? binding.recorded : binding.live;
      const to = direction === 'response' ? binding.live : binding.recorded;
      const actual = parent[property];
      if (mode === 'exact') {
        if (actual !== from) fail('binding_value_mismatch', `绑定字段不符合其声明身份: ${rule.path}`);
        parent[property] = to;
      } else if (mode === 'path-prefix' && binding.kind === 'path') {
        if (typeof actual !== 'string' || path.normalize(actual) !== actual ||
            (actual !== from && !actual.startsWith(`${from}/`))) {
          fail('binding_value_mismatch', `路径不属于声明的工作区: ${rule.path}`);
        }
        parent[property] = to + actual.slice(from.length);
      } else {
        fail('invalid_binding_mode', '只接受 exact、typed-template，或 path 类型的 path-prefix');
      }
    }
    return result;
  }

  for (const declaration of declarations) declare(declaration);
  return Object.freeze({
    runId,
    declare,
    bind,
    normalizeRequest: (body, options) => transform(body, options, 'request'),
    rebindResponse: (body, options) => transform(body, options, 'response'),
    snapshot: () => [...records.values()].map((record) => ({ ...record })),
  });
}

export function normalizeBody(body, registry, owner, rules = []) {
  return registry.normalizeRequest(body, { owner, rules });
}

export { derivePortableSse } from './portable-sse.mjs';
