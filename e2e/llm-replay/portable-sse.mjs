import { createHash } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { BindingError } from './binding-error.mjs';
import { canonicalJson, parseStrictJson } from './matcher.mjs';

function fail(code, message) {
  throw new BindingError(code, message);
}

function object(value, label) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    fail('invalid_tool_arguments', `${label} 必须是 JSON object`);
  }
  return value;
}

function argumentObject(text) {
  if (typeof text !== 'string') fail('invalid_tool_arguments', '工具参数必须是完整 JSON 字符串');
  try {
    const strict = parseStrictJson(Buffer.from(text));
    return { ordinary: object(JSON.parse(text), '工具参数'), strict, text, canonical: canonicalJson(strict) };
  } catch (error) {
    if (error instanceof BindingError) throw error;
    if (error.code === 'duplicate_json_key') fail('duplicate_tool_argument_key', '工具参数包含重复 JSON key，不能自动修正');
    fail('incomplete_tool_arguments', '不能补全、猜测或改写残缺的工具参数 JSON');
  }
}

function readFrames(raw) {
  let text;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(raw);
  } catch {
    fail('incomplete_sse', 'portable SSE 必须包含完整 UTF-8 字节');
  }
  const frames = [];
  let offset = 0;
  for (const match of text.matchAll(/\r\n\r\n|\n\n|\r\r/gu)) {
    const chunk = text.slice(offset, match.index);
    offset = match.index + match[0].length;
    const lines = chunk.split(/\r\n|\n|\r/u);
    const payload = lines.filter((line) => line.startsWith('data:')).map((line) => line.slice(5).replace(/^ /u, '')).join('\n');
    const eventLine = lines.find((line) => line.startsWith('event:'));
    let data = null;
    let strict = null;
    if (payload && payload !== '[DONE]') {
      try { strict = parseStrictJson(Buffer.from(payload)); data = JSON.parse(payload); } catch {
        fail('invalid_sse_json', 'portable SSE 不接受非 JSON 数据帧');
      }
      object(data, 'SSE data');
    }
    frames.push({ raw: chunk + match[0], lines, separator: match[0], data, strict,
      event: eventLine?.slice(6).trim(), sentinel: payload === '[DONE]' });
  }
  if (offset !== text.length) fail('incomplete_sse', 'portable SSE 不能补全末尾未结束的数据帧');
  return frames;
}

function mergeChangedStrings(strict, before, after) {
  if (isDeepStrictEqual(before, after)) return strict;
  if (typeof before === 'string' && typeof after === 'string') return after;
  if (before && after && typeof before === 'object' && typeof after === 'object' &&
      Array.isArray(before) === Array.isArray(after) && isDeepStrictEqual(Object.keys(before).sort(), Object.keys(after).sort())) {
    const result = Array.isArray(before) ? [] : Object.create(null);
    for (const key of Object.keys(before)) result[key] = mergeChangedStrings(strict[key], before[key], after[key]);
    return result;
  }
  fail('unsupported_portable_rewrite', 'portable 只允许已声明字符串叶节点的替换');
}

/**
 * JSON inside a protocol string is observable history: key order, whitespace and
 * untouched numeric literals must survive a portable rewrite. The input has
 * already passed parseStrictJson; only changed string VALUE spans are replaced.
 */
function replaceArgumentStrings(text, before, after) {
  let offset = 0;
  const changes = [];
  const space = () => { while (offset < text.length && /[\x20\t\r\n]/u.test(text[offset])) offset++; };
  const string = () => {
    const start = offset++;
    while (offset < text.length) {
      const character = text[offset++];
      if (character === '\\') offset++;
      else if (character === '"') return { start, end: offset, value: JSON.parse(text.slice(start, offset)) };
    }
    fail('invalid_tool_arguments', '已验证参数的字符串边界不完整');
  };
  const visit = (oldValue, newValue) => {
    space();
    if (text[offset] === '"') {
      const span = string();
      if (oldValue !== newValue) changes.push({ ...span, replacement: JSON.stringify(newValue) });
    } else if (text[offset] === '{') {
      offset++; space();
      if (text[offset] === '}') { offset++; return; }
      while (offset < text.length) {
        const key = string().value;
        space(); offset++; // colon; syntax and duplicate keys were validated above
        visit(oldValue[key], newValue[key]); space();
        if (text[offset++] === '}') return;
        space();
      }
    } else if (text[offset] === '[') {
      offset++; space();
      if (text[offset] === ']') { offset++; return; }
      let index = 0;
      while (offset < text.length) {
        visit(oldValue[index], newValue[index]); index++; space();
        if (text[offset++] === ']') return;
        space();
      }
    } else {
      const literal = /^(?:true|false|null|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)/u.exec(text.slice(offset));
      if (!literal) fail('invalid_tool_arguments', '已验证参数的字面量边界不完整');
      offset += literal[0].length;
    }
  };
  visit(before, after); space();
  if (offset !== text.length) fail('invalid_tool_arguments', '工具参数词法替换未完整消费 JSON');
  let cursor = 0;
  let result = '';
  for (const change of changes) {
    result += text.slice(cursor, change.start) + change.replacement;
    cursor = change.end;
  }
  return result + text.slice(cursor);
}

function rebindArguments(args, registry, owner, rules) {
  const ordinary = registry.rebindResponse(args.ordinary, { owner, rules });
  // Reject any structural/non-string change before applying lexical replacements.
  mergeChangedStrings(args.strict, args.ordinary, ordinary);
  return { ordinary, serialized: replaceArgumentStrings(args.text, args.ordinary, ordinary) };
}

function render(frame, data) {
  if (frame.lines.some((line) => line.startsWith('id:') || line.startsWith('retry:'))) {
    fail('unsupported_sse_metadata', '重分片不支持带 id/retry 的目标帧');
  }
  if (frame.event && frame.event !== data.type) {
    fail('inconsistent_sse_event', 'SSE event 与 data.type 不一致');
  }
  let wrote = false;
  const lines = frame.lines.flatMap((line) => {
    if (!line.startsWith('data:')) return [line];
    if (wrote) return [];
    wrote = true;
    return [`data: ${canonicalJson(mergeChangedStrings(frame.strict, frame.data, data))}`];
  });
  const newline = frame.separator.startsWith('\r\n') ? '\r\n' : frame.separator.startsWith('\r') ? '\r' : '\n';
  return lines.join(newline) + frame.separator;
}

function fragment(text, plan) {
  const result = [];
  let part = '';
  let size = 0;
  for (const character of text) {
    const bytes = Buffer.byteLength(character);
    if (size + bytes > plan.maxBytes && part) {
      result.push(part);
      part = '';
      size = 0;
    }
    part += character;
    size += bytes;
  }
  if (part) result.push(part);
  return result;
}

function sameArguments(values, deltas) {
  const candidates = values.map(argumentObject);
  if (deltas.length) candidates.unshift(argumentObject(deltas.join('')));
  if (!candidates.length) fail('incomplete_tool_arguments', '缺少完整工具参数');
  if (candidates.some((value) => value.canonical !== candidates[0].canonical)) {
    fail('inconsistent_tool_arguments', '同一工具调用的 delta 与终态参数副本不一致');
  }
  return candidates[0];
}

function rulesByTool(tools) {
  if (!Array.isArray(tools) || tools.length === 0) fail('invalid_portable_rules', '必须显式指定需要重绑定的工具调用');
  const result = new Map();
  for (const tool of tools) {
    if (!tool || typeof tool.toolCallId !== 'string' || !tool.toolCallId || !Array.isArray(tool.rules) || !tool.rules.length) {
      fail('invalid_portable_rules', '每条工具规则需要 toolCallId 和非空 rules');
    }
    if (result.has(tool.toolCallId)) fail('duplicate_tool_rule', '同一工具调用只能声明一次');
    result.set(tool.toolCallId, tool.rules);
  }
  return result;
}

/** 完整原始响应中的工具参数，用于录制后生成可审查的 typed 字段规则。 */
export function inspectPortableTools({ raw, protocol } = {}) {
  const frames = readFrames(Buffer.from(raw));
  if (protocol === 'anthropic') {
    const tools = new Map();
    let terminal = false;
    for (const frame of frames) {
      const data = frame.data;
      if (!data) continue;
      if (terminal) fail('events_after_terminal', 'message_stop 后还有数据帧');
      if (data.type === 'message_stop') { terminal = true; continue; }
      if (data.type === 'content_block_start' && data.content_block?.type === 'tool_use') {
        if (tools.has(data.index)) fail('duplicate_tool_item', '工具 block index 重复');
        tools.set(data.index, { toolCallId: data.content_block.id, name: data.content_block.name,
          initial: canonicalJson(frame.strict.content_block.input), deltas: [], stopped: false });
      } else if (data.type === 'content_block_delta' && tools.has(data.index)) {
        const tool = tools.get(data.index);
        if (tool.stopped || data.delta?.type !== 'input_json_delta' || typeof data.delta.partial_json !== 'string') {
          fail('unsupported_tool_event', '目标工具包含未知或已结束的 delta');
        }
        tool.deltas.push(data.delta.partial_json);
      } else if (data.type === 'content_block_stop' && tools.has(data.index)) {
        const tool = tools.get(data.index);
        if (tool.stopped) fail('duplicate_tool_item', '目标工具重复结束');
        tool.stopped = true;
      }
    }
    if (!terminal) fail('incomplete_sse', '工具发现只接受完整 message_stop');
    return [...tools.values()].map((tool) => {
      if (!tool.stopped) fail('incomplete_tool_arguments', '目标工具缺少 content_block_stop');
      const initial = tool.initial !== '{}' || !tool.deltas.length ? [tool.initial] : [];
      return { toolCallId: tool.toolCallId, name: tool.name, arguments: sameArguments(initial, tool.deltas).ordinary };
    });
  }
  if (protocol === 'openai-responses') {
    const terminal = frames.find((frame) => frame.data?.type === 'response.completed');
    if (!Array.isArray(terminal?.data?.response?.output)) fail('incomplete_sse', '工具发现需要完整 response.completed.output');
    return terminal.data.response.output.filter((item) => item.type === 'function_call').map((item) => ({
      toolCallId: item.call_id, name: item.name, arguments: argumentObject(item.arguments).ordinary,
    }));
  }
  fail('unsupported_portable_protocol', `不支持的工具发现协议: ${protocol}`);
}

function replaceDeltas(frames, replacements, indices, makeData, text, plan) {
  if (!indices.length) return;
  const first = indices[0];
  // 派生 fixture 明确集中替换第一个 delta；其余原始帧相对顺序保持不变。
  replacements.set(first, fragment(text, plan).map((part) => render(frames[first], makeData(part))).join(''));
  for (const index of indices.slice(1)) replacements.set(index, '');
}

function anthropic(frames, registry, owner, toolRules, plan) {
  const targets = new Map();
  const indexes = new Map();
  const replacements = new Map();
  let terminal = false;
  for (let i = 0; i < frames.length; i += 1) {
    const data = frames[i].data;
    if (!data) continue;
    if (terminal) fail('events_after_terminal', 'message_stop 后还有数据帧');
    if (data.type === 'message_stop') { terminal = true; continue; }
    if (data.type === 'content_block_start' && data.content_block?.type === 'tool_use') {
      const block = data.content_block;
      if (indexes.has(data.index)) fail('duplicate_tool_item', '工具 block index 重复');
      indexes.set(data.index, block.id);
      if (!toolRules.has(block.id)) continue;
      if (targets.has(block.id)) fail('duplicate_tool_item', 'tool_use id 重复');
      targets.set(block.id, { start: i, index: data.index, initial: object(block.input, 'tool_use.input'), deltas: [], stopped: false });
    } else if (data.type === 'content_block_delta') {
      const target = targets.get(indexes.get(data.index));
      if (!target) continue;
      if (target.stopped || data.delta?.type !== 'input_json_delta' || typeof data.delta.partial_json !== 'string') {
        fail('unsupported_tool_event', '目标工具包含未知、越界或已结束的 delta');
      }
      target.deltas.push(i);
    } else if (data.type === 'content_block_stop') {
      const target = targets.get(indexes.get(data.index));
      if (target) {
        if (target.stopped) fail('duplicate_tool_item', '目标工具重复结束');
        target.stopped = true;
      }
    } else if (data.type === 'message_start' && data.message?.content?.length) {
      fail('unsupported_tool_event', 'portable 模式不支持 message_start 内嵌的完整 content');
    }
  }
  if (!terminal) fail('incomplete_sse', 'portable Anthropic fixture 需要完整 message_stop');
  for (const [toolCallId, rules] of toolRules) {
    const target = targets.get(toolCallId);
    if (!target) fail('unknown_tool_reference', `未找到声明的工具调用: ${toolCallId}`);
    if (!target.stopped) fail('incomplete_tool_arguments', '目标工具缺少 content_block_stop');
    const deltas = target.deltas.map((index) => frames[index].data.delta.partial_json);
    const initial = Object.keys(target.initial).length || !deltas.length
      ? [canonicalJson(frames[target.start].strict.content_block.input)] : [];
    const args = sameArguments(initial, deltas);
    const rebound = rebindArguments(args, registry, owner, rules);
    if (initial.length) {
      const data = structuredClone(frames[target.start].data);
      data.content_block.input = rebound.ordinary;
      replacements.set(target.start, render(frames[target.start], data));
    }
    replaceDeltas(frames, replacements, target.deltas,
      (part) => ({ ...frames[target.deltas[0]].data, delta: { type: 'input_json_delta', partial_json: part } }),
      rebound.serialized, plan);
  }
  return replacements;
}

function responses(frames, registry, owner, toolRules, plan) {
  const items = new Map();
  const byIndex = new Map();
  const byCall = new Map();
  const replacements = new Map();
  const modified = new Map();
  let terminal = false;

  function remember(item, frame, pointer, index, final) {
    if (item?.type !== 'function_call') return;
    if (typeof item.id !== 'string' || !item.id || typeof item.call_id !== 'string' || !item.call_id) {
      fail('invalid_tool_identity', 'function_call 必须有 item id 和 call_id');
    }
    if (!toolRules.has(item.call_id)) return;
    let target = items.get(item.id);
    if (!target) {
      if (byCall.has(item.call_id)) fail('duplicate_tool_item', '同一 call_id 指向多个 items');
      target = { id: item.id, callId: item.call_id, name: item.name, copies: [], deltas: [], doneArgs: [] };
      items.set(item.id, target);
      byCall.set(item.call_id, target);
    }
    if (target.callId !== item.call_id || target.name !== item.name) fail('invalid_tool_identity', '工具 item 身份发生变化');
    if (index !== undefined) {
      if (!Number.isSafeInteger(index) || index < 0 || (byIndex.has(index) && byIndex.get(index) !== item.id)) {
        fail('invalid_tool_identity', 'output_index 与工具 item 不一致');
      }
      byIndex.set(index, item.id);
    }
    if (typeof item.arguments !== 'string') fail('invalid_tool_arguments', 'function_call.arguments 必须是字符串');
    if (final || item.arguments !== '') target.copies.push({ frame, pointer, value: item.arguments });
  }

  for (let i = 0; i < frames.length; i += 1) {
    const data = frames[i].data;
    if (!data) continue;
    if (terminal) fail('events_after_terminal', 'response.completed 后还有数据帧');
    if (data.type === 'response.output_item.added' || data.type === 'response.output_item.done') {
      remember(data.item, i, ['item', 'arguments'], data.output_index, data.type.endsWith('.done'));
    } else if (data.type === 'response.function_call_arguments.delta' || data.type === 'response.function_call_arguments.done') {
      const target = items.get(data.item_id ?? byIndex.get(data.output_index));
      if (!target) {
        if (data.item_id === undefined && data.output_index === undefined) fail('invalid_tool_identity', '参数事件缺少 item 归属');
        continue;
      }
      if (data.output_index !== undefined && byIndex.get(data.output_index) !== target.id) {
        fail('invalid_tool_identity', '参数事件的 item_id 与 output_index 不一致');
      }
      if (data.type.endsWith('.delta')) {
        if (target.doneArgs.length || typeof data.delta !== 'string') fail('unsupported_tool_event', '目标参数结束后不能再有 delta');
        target.deltas.push(i);
      } else {
        target.doneArgs.push({ frame: i, pointer: ['arguments'], value: data.arguments });
      }
    } else if (data.type === 'response.completed') {
      if (!Array.isArray(data.response?.output)) fail('incomplete_sse', 'response.completed 缺少 output 数组');
      for (let index = 0; index < data.response.output.length; index += 1) {
        remember(data.response.output[index], i, ['response', 'output', index, 'arguments'], undefined, true);
      }
      terminal = true;
    } else if (items.has(data.item_id) || (data.item?.type === 'function_call' && toolRules.has(data.item.call_id))) {
      fail('unsupported_tool_event', '目标工具出现未知协议事件，不能猜测其参数副本');
    }
  }
  if (!terminal) fail('incomplete_sse', 'portable Responses fixture 需要完整 response.completed');
  for (const [toolCallId, rules] of toolRules) {
    const target = byCall.get(toolCallId);
    if (!target) fail('unknown_tool_reference', `未找到声明的工具调用: ${toolCallId}`);
    const copies = [...target.copies, ...target.doneArgs];
    const args = sameArguments(copies.map((copy) => copy.value), target.deltas.map((index) => frames[index].data.delta));
    const text = rebindArguments(args, registry, owner, rules).serialized;
    for (const copy of copies) {
      if (!modified.has(copy.frame)) modified.set(copy.frame, structuredClone(frames[copy.frame].data));
      let parent = modified.get(copy.frame);
      for (const token of copy.pointer.slice(0, -1)) parent = parent[token];
      // Equivalent protocol copies may have different JSON key order. Preserve
      // each copy's own lexical form while applying the same typed replacements.
      parent[copy.pointer.at(-1)] = rebindArguments(argumentObject(copy.value), registry, owner, rules).serialized;
    }
    replaceDeltas(frames, replacements, target.deltas,
      (part) => ({ ...frames[target.deltas[0]].data, delta: part }), text, plan);
  }
  for (const [index, data] of modified) replacements.set(index, render(frames[index], data));
  return replacements;
}

/** 仅派生完整工具参数；raw 与 signed/opaque 帧保持原件，不推断截断后缀。 */
export function derivePortableSse({ raw, protocol, registry, owner, tools, rulesVersion, fragmentPlan } = {}) {
  if (!(typeof raw === 'string' || Buffer.isBuffer(raw) || raw instanceof Uint8Array)) {
    fail('invalid_portable_source', 'raw 必须是原始 SSE 字节或字符串');
  }
  if (typeof rulesVersion !== 'string' || !rulesVersion) fail('invalid_portable_rules', '派生 fixture 必须声明规则版本');
  if (fragmentPlan?.kind !== 'utf8' || !Number.isSafeInteger(fragmentPlan.maxBytes) || fragmentPlan.maxBytes < 4 || fragmentPlan.maxBytes > 65536) {
    fail('invalid_fragment_plan', '必须显式声明 utf8 分片计划，maxBytes 为 4..65536');
  }
  if (!registry || typeof registry.rebindResponse !== 'function') fail('invalid_binding_registry', '缺少运行期绑定表');
  const bytes = Buffer.from(raw);
  const frames = readFrames(bytes);
  const rules = rulesByTool(tools);
  let replacements;
  if (protocol === 'anthropic') replacements = anthropic(frames, registry, owner, rules, fragmentPlan);
  else if (protocol === 'openai-responses') replacements = responses(frames, registry, owner, rules, fragmentPlan);
  else fail('unsupported_portable_protocol', `该协议只能 raw 重放: ${protocol}`);
  const body = Buffer.from(frames.map((frame, index) => replacements.get(index) ?? frame.raw).join(''));
  return {
    body,
    provenance: {
      formatVersion: 1,
      mode: 'portable',
      protocol,
      sourceSha256: createHash('sha256').update(bytes).digest('hex'),
      derivedSha256: createHash('sha256').update(body).digest('hex'),
      rulesVersion,
      fragmentPlan: { ...fragmentPlan, placement: 'replace-first-delta' },
      timing: 'not-preserved',
      rawChunkBoundaries: 'not-preserved',
    },
  };
}
