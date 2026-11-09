/**
 * world 快照与当前源码之间的显式映射。迁移、world:sync 与 drift 校验共用同一份定义，
 * 避免“哪个 leaf 可以变化”出现两套口径。
 */
import { AGENT_LEAF_PREFIX, STABLE_LEAVES } from '../llm-replay/world-mapping.mjs';

export { AGENT_LEAF_PREFIX, STABLE_LEAVES };

export const SOURCE_FILES = Object.freeze({
  skills: 'peri-acp/prompts/sections/13_skills.md',
  agentDescription: 'peri-middlewares/src/subagent/tool/descriptions/agent.md',
  agentSchema: 'peri-middlewares/src/subagent/tool/define.rs',
  compactSummary: 'peri-agent/src/agent/compact_v2/descriptions/summary_system_prompt.md',
});

/** Agent 工具 description 的稳定来源；其他 tool 文本仍是录制内容，不做源码同步。 */
export function agentDescriptionLeafIds(world) {
  const ids = world.leaves.filter((id) => id.startsWith(AGENT_LEAF_PREFIX));
  if (!ids.length) throw new Error(`world 缺少 ${AGENT_LEAF_PREFIX}* leaf，无法映射 Agent description`);
  return ids;
}

/**
 * block1 的前缀语义：主 Agent（含/不含 replay-worker catalog）与子 Agent 的短 prompt。
 * 分类冲突时由调用方显式失败，而不是静默合并到同一个 leaf。
 */
export function classifyBlock1Prefix(prefix) {
  if (prefix.startsWith('Complete the delegated task with the available workspace tools.')) return 'subagent';
  if (prefix.includes('replay-worker [')) return 'replay-worker';
  return 'main';
}

const SEND_ONLY = Object.freeze({
  type: 'boolean',
  default: false,
  description: 'When true, only queue a non-empty prompt to the live background sub-agent identified by resume_thread_id. If it has no live receiver, fail without loading or resuming a persisted thread.',
});

/** 复用旧生成器的契约检查：define.rs 的 send_only 定义变化时必须显式失败。 */
export function expectedSendOnly(defineSource) {
  const field = defineSource.match(/"send_only": \{([\s\S]*?)\n\s*\},/u)?.[1] ?? '';
  if (!field.includes('"type": "boolean"') || !field.includes('"default": false')
      || !field.includes(JSON.stringify(SEND_ONLY.description))) {
    throw new Error('send_only fixture schema does not match current define.rs contract');
  }
  return structuredClone(SEND_ONLY);
}

/** Agent tool 对象可能是 Anthropic 的 input_schema，也可能是 Responses 的 parameters。 */
export function agentSchemaOf(tool) {
  const schema = tool.input_schema ?? tool.parameters;
  if (!schema || typeof schema !== 'object' || Array.isArray(schema)
      || !schema.properties || typeof schema.properties !== 'object' || Array.isArray(schema.properties)) {
    throw new Error('Agent tool 缺少可更新的 input_schema/parameters.properties');
  }
  return schema;
}
