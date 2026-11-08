import { createHash } from 'node:crypto';

export class ReplayError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = 'ReplayError';
    this.code = code;
    this.details = details;
  }
}

export function sha256(value) {
  return createHash('sha256').update(value).digest('hex');
}

const ID_FIELDS = ['owner', 'execution', 'call', 'attempt', 'purpose'];

export function validateContext(context) {
  if (!context || typeof context !== 'object' || Array.isArray(context)) {
    throw new ReplayError('invalid_context', '缺少对象类型的调用身份');
  }
  if (context.v !== undefined && context.v !== 1) {
    throw new ReplayError('invalid_context', '不支持的调用身份版本');
  }
  for (const field of ['run', 'owner', 'execution', 'call', 'purpose']) {
    if (typeof context[field] !== 'string' || !context[field] || context[field].length > 1024) {
      throw new ReplayError('invalid_context', `调用身份字段 ${field} 必须为非空字符串`);
    }
  }
  if (!Number.isSafeInteger(context.attempt) || context.attempt < 1) {
    throw new ReplayError('invalid_context', 'attempt 必须为正整数');
  }
  if (context.generation !== undefined && typeof context.generation !== 'string') {
    throw new ReplayError('invalid_context', 'generation 必须为字符串');
  }
  if (context.parent !== undefined) {
    const parent = context.parent;
    if (!parent || typeof parent !== 'object' || Array.isArray(parent)
        || !['owner', 'execution', 'call', 'tool_call'].every((field) => typeof parent[field] === 'string' && parent[field])
        || (parent.slot !== undefined && !((Number.isSafeInteger(parent.slot) && parent.slot >= 0) || (typeof parent.slot === 'string' && parent.slot.length > 0)))
        || Object.keys(parent).some((field) => !['owner', 'execution', 'call', 'tool_call', 'slot'].includes(field))) {
      throw new ReplayError('invalid_parent', 'parent 必须携带合法的调用与工具因果身份');
    }
  }
  return context;
}

export function decodeContext(header) {
  if (typeof header !== 'string' || !/^[A-Za-z0-9_-]+$/.test(header) || header.length > 16384) {
    throw new ReplayError('invalid_context', '缺少合法 x-peri-replay-context');
  }
  try {
    const bytes = Buffer.from(header, 'base64url');
    if (bytes.toString('base64url') !== header) throw new Error('noncanonical base64url');
    return validateContext(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)));
  } catch (error) {
    if (error instanceof ReplayError) throw error;
    throw new ReplayError('invalid_context', '调用身份无法解析');
  }
}

export function identityOf(context) {
  return {
    ...Object.fromEntries(ID_FIELDS.map((field) => [field, context[field]])),
    ...(context.parent ? { parent: { ...context.parent } } : {}),
  };
}

export function identityKey(context) {
  return JSON.stringify(ID_FIELDS.map((field) => context[field]));
}

export function logicalTarget(value) {
  let target;
  try { target = new URL(value); } catch { throw new ReplayError('invalid_target', '缺少完整逻辑请求 URL'); }
  if (!['http:', 'https:'].includes(target.protocol) || target.username || target.password || target.hash) {
    throw new ReplayError('invalid_target', '逻辑请求 URL 必须为无用户名、密码和 fragment 的 HTTP(S) URL');
  }
  const credentialParameters = new Set(['api_key', 'api-key', 'apikey', 'access_token', 'token', 'key', 'authorization', 'password', 'secret', 'signature', 'credential']);
  if ([...target.searchParams.keys()].some((name) => credentialParameters.has(name.toLowerCase()))) {
    throw new ReplayError('credential_in_target', '认证信息必须放在不落盘的请求 header，不能放入录制 URL');
  }
  return target.href;
}

// 只记录与协议语义相关的安全 header；认证 header 始终不进入 cassette。
const SEMANTIC_HEADERS = ['content-type', 'anthropic-version', 'anthropic-beta', 'openai-beta'];
export function semanticHeaders(headers) {
  return Object.fromEntries(SEMANTIC_HEADERS.filter((name) => headers[name] !== undefined)
    .map((name) => [name, String(headers[name])]));
}

class JsonNumber { constructor(raw) { this.raw = raw; } }

/** 拒绝重复键并保留数值字面量，避免 JS 浮点舍入使不同请求误匹配。 */
export function parseStrictJson(bytes) {
  let text;
  try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
  catch { throw new ReplayError('invalid_json', '请求不是合法 UTF-8'); }
  let pos = 0;
  const fail = () => { throw new ReplayError('invalid_json', `JSON 无效，偏移 ${pos}`); };
  const space = () => { while (pos < text.length && /[\x20\t\r\n]/.test(text[pos])) pos++; };
  const string = () => {
    const start = pos++;
    while (pos < text.length) {
      const c = text[pos++];
      if (c === '\\') pos++;
      else if (c === '"') {
        try { return JSON.parse(text.slice(start, pos)); } catch { fail(); }
      }
    }
    fail();
  };
  const value = (depth = 0) => {
    if (depth > 256) fail();
    space();
    if (text[pos] === '"') return string();
    if (text[pos] === '{') {
      pos++; space();
      const result = Object.create(null);
      if (text[pos] === '}') { pos++; return result; }
      while (pos < text.length) {
        if (text[pos] !== '"') fail();
        const key = string();
        if (Object.hasOwn(result, key)) throw new ReplayError('duplicate_json_key', `JSON 重复键 ${key}`);
        space(); if (text[pos++] !== ':') fail();
        result[key] = value(depth + 1); space();
        if (text[pos] === '}') { pos++; return result; }
        if (text[pos++] !== ',') fail();
        space();
      }
      fail();
    }
    if (text[pos] === '[') {
      pos++; space();
      const result = [];
      if (text[pos] === ']') { pos++; return result; }
      while (pos < text.length) {
        result.push(value(depth + 1)); space();
        if (text[pos] === ']') { pos++; return result; }
        if (text[pos++] !== ',') fail();
      }
      fail();
    }
    for (const [token, result] of [['true', true], ['false', false], ['null', null]]) {
      if (text.startsWith(token, pos)) { pos += token.length; return result; }
    }
    const number = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/.exec(text.slice(pos));
    if (number) { pos += number[0].length; return new JsonNumber(number[0]); }
    fail();
  };
  const result = value(); space();
  if (pos !== text.length) fail();
  return result;
}

export function canonicalJson(value) {
  if (value instanceof JsonNumber) return value.raw;
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

function pointer(root, path, replacement, write) {
  if (typeof path !== 'string' || !path.startsWith('/')) throw new ReplayError('invalid_binding_rule', '绑定须使用非根 JSON Pointer');
  const parts = path.slice(1).split('/').map((part) => part.replace(/~1/g, '/').replace(/~0/g, '~'));
  let node = root;
  for (const part of parts.slice(0, -1)) {
    if (!node || typeof node !== 'object' || !Object.hasOwn(node, part)) throw new ReplayError('invalid_binding_rule', `绑定路径不存在：${path}`);
    node = node[part];
  }
  const last = parts.at(-1);
  if (!node || typeof node !== 'object' || !Object.hasOwn(node, last)) throw new ReplayError('invalid_binding_rule', `绑定路径不存在：${path}`);
  if (write) node[last] = replacement;
  return node[last];
}

export function canonicalBody(bytes, { bindings, owner, rules = [] } = {}) {
  const tree = parseStrictJson(bytes);
  if (rules.length) {
    if (!bindings) throw new ReplayError('missing_bindings', '请求声明了绑定规则，但未注册绑定');
    const ordinary = JSON.parse(bytes.toString('utf8'));
    const normalized = bindings.normalizeRequest(ordinary, { owner, rules });
    // 仅替换显式声明的叶节点；未声明数值仍使用原始字面量。
    for (const rule of rules) pointer(tree, rule.path, pointer(normalized, rule.path), true);
  }
  return canonicalJson(tree);
}

export function assertRequestMatches(expected, actual, normalization = {}) {
  const actualCanonical = canonicalBody(actual.body, normalization);
  const differences = [];
  for (const field of ['method', 'target']) if (expected[field] !== actual[field]) differences.push(field);
  if (canonicalJson(expected.headers) !== canonicalJson(actual.headers)) differences.push('headers');
  if (expected.canonicalBody !== actualCanonical) differences.push('body');
  if (differences.length) throw new ReplayError('request_mismatch', '当前请求与录制不一致', {
    fields: differences,
    expectedBodyHash: sha256(expected.canonicalBody),
    actualBodyHash: sha256(actualCanonical),
  });
}
