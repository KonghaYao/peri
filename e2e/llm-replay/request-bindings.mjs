import { posix as path } from 'node:path';
import { createBindingRegistry } from './bindings.mjs';
import { BindingError } from './binding-error.mjs';
import { identityKey, parseStrictJson } from './matcher.mjs';
import { loadCassette, readResponseChunks } from './cassette.mjs';
import { derivePortableSse, inspectPortableTools } from './portable-sse.mjs';
import { templateFor } from './template-text.mjs';

const PROTECTED_KEYS = new Set(['signature', 'encrypted_content', 'provider_state', 'providerState', 'reasoning_content', 'thoughtSignature', 'thought_signature']);
const PROTECTED_TYPES = new Set(['thinking', 'redacted_thinking', 'reasoning', 'redacted_reasoning', 'provider_state']);
const referenceKey = ({ owner, kind, alias }) => JSON.stringify([owner, kind, alias]);
const pointerToken = (text) => text.replace(/~/gu, '~0').replace(/\//gu, '~1');
const fail = (code, message) => { throw new BindingError(code, message); };

/** 录制时只从已绑定身份发现精确字段模板；重放时绝不重新推断可忽略字段。 */
export function discoverBindingRules(body, { registry, owner = 'world', source = 'live', paths } = {}) {
  if (!['live', 'recorded'].includes(source)) fail('invalid_binding_source', 'source 必须为 live 或 recorded');
  const bindings = registry.snapshot().filter((binding) => binding.owner === owner);
  const allowed = paths === undefined ? null : new Set(paths);
  const rules = [];
  const visit = (value, pointer, systemText = false) => {
    if (typeof value === 'string') {
      if (allowed && !allowed.has(pointer)) return;
      const segments = templateFor(value, bindings, source, systemText);
      if (segments) rules.push({ path: pointer, mode: 'typed-template', segments, systemText });
    } else if (value && typeof value === 'object') {
      if (PROTECTED_TYPES.has(value.type) || PROTECTED_TYPES.has(value.kind)) return;
      for (const [key, child] of Object.entries(value)) {
        if (PROTECTED_KEYS.has(key)) continue;
        const isSystem = systemText || (pointer === '' && ['system', 'instructions'].includes(key)) ||
          (key === 'content' && ['system', 'developer'].includes(value.role));
        visit(child, `${pointer}/${pointerToken(key)}`, isSystem);
      }
    }
  };
  visit(body, '');
  return rules;
}

const DEFAULT_TOOL_FIELDS = {
  Write: ['/file_path'], Edit: ['/file_path'], Read: ['/file_path'],
  Agent: ['/resume_thread_id', '/prompt'], folder_operations: ['/path'],
};

/**
 * 每个真实 run 独立创建。fixture 只持有 recorded 声明和模板；live world/session 由 runner 登记。
 * 响应仅从完整工具参数的显式白名单字段派生；不会改写普通回复、签名或原始 cassette。
 */
export function createRequestBindingAdapter({ mode, runId, declarations = [], owner = 'world', bindings } = {}) {
  if (!['record', 'replay'].includes(mode)) fail('invalid_binding_mode', 'adapter mode 必须为 record 或 replay');
  const registry = bindings ?? createBindingRegistry({ runId, declarations });
  if (registry.runId !== runId) fail('binding_run_mismatch', '每个 run 必须有独立绑定表');
  if (bindings && declarations.length) fail('invalid_binding_registry', '不能同时传 bindings 和 declarations');
  const rulesByCall = new Map();

  const declareAndBind = (input) => {
    const value = { owner, ...input };
    let declared = registry.snapshot().find((entry) => referenceKey(entry) === referenceKey(value));
    if (!declared) {
      if (mode !== 'record') fail('binding_not_declared', 'replay 不允许添加 fixture 未声明的身份');
      declared = registry.declare({ ...value, recorded: value.recorded ?? value.live });
    } else if (value.recorded !== undefined && value.recorded !== declared.recorded) {
      fail('binding_alias_rebound', 'recorded 声明不能在运行期替换');
    }
    return registry.bind(value);
  };

  return Object.freeze({
    registry,
    declareAndBind,
    bindingOwnerFor: () => owner,
    onRequest({ context, request }) {
      if (mode !== 'record') return;
      parseStrictJson(request.body);
      const body = JSON.parse(request.body.toString('utf8'));
      const rules = discoverBindingRules(body, { registry, owner });
      rulesByCall.set(identityKey(context), rules);
    },
    requestRules(context) {
      if (mode !== 'record') fail('read_only_binding_rules', 'replay 规则必须来自封存的 cassette');
      const rules = rulesByCall.get(identityKey(context));
      if (!rules) fail('binding_rules_missing', '必须先通过 onRequest 捕获本次录制的精确字段');
      return structuredClone(rules);
    },
    snapshotDeclarations() {
      return registry.snapshot().map(({ live, ...declaration }) => declaration);
    },
    async responsePlans(cassetteDir, { tools = DEFAULT_TOOL_FIELDS, rulesVersion = 'world-bindings-v1' } = {}) {
      const cassette = await loadCassette(cassetteDir);
      const plans = [];
      for (const entry of cassette.entries.values()) {
        const response = entry.response;
        const protocol = response.protocolFacts?.protocol;
        if (response.terminal.kind !== 'complete' || !['message_stop', 'response.completed'].includes(response.protocolFacts?.marker) ||
            !['anthropic', 'openai-responses'].includes(protocol)) continue;
        const parts = [];
        for await (const part of readResponseChunks(cassette, entry)) parts.push(part.bytes);
        const raw = Buffer.concat(parts);
        const targets = inspectPortableTools({ raw, protocol });
        const selected = targets.flatMap((tool) => {
          const paths = tools[tool.name];
          if (!paths) return [];
          const strictPaths = new Set(paths.filter((pointer) => ['/file_path', '/path', '/cwd'].includes(pointer)));
          const rules = discoverBindingRules(tool.arguments, { registry, owner, source: 'recorded',
            paths: paths.filter((pointer) => !strictPaths.has(pointer)) });
          for (const pointer of strictPaths) {
            const value = tool.arguments[pointer.slice(1)];
            if (typeof value !== 'string') continue;
            const binding = registry.snapshot().filter((item) => item.owner === owner && item.kind === 'path' &&
              (value === item.recorded || value.startsWith(`${item.recorded}/`)))
              .sort((left, right) => right.recorded.length - left.recorded.length)[0];
            if (!binding) continue;
            if (path.normalize(value) !== value) fail('binding_value_mismatch', `工具路径不规范或越出声明根: ${pointer}`);
            rules.push({ path: pointer, kind: 'path', alias: binding.alias, mode: 'path-prefix' });
          }
          return rules.length ? [{ toolCallId: tool.toolCallId, rules }] : [];
        });
        if (!selected.length) continue;
        const plan = { identity: entry.identity, protocol, owner, tools: selected, rulesVersion,
          fragmentPlan: { kind: 'utf8', maxBytes: 128 } };
        const derived = derivePortableSse({ ...plan, raw, registry });
        plans.push({ ...plan, sourceSha256: derived.provenance.sourceSha256 });
      }
      return plans;
    },
  });
}
