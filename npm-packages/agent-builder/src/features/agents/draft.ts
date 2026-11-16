import type { Agent, AgentDraft } from '@shared/agent';

/** 应用官方模板不覆盖运行时默认 Prompt/能力，也不包含部署配置。 */
export function officialDefaultDraft(current: AgentDraft): AgentDraft {
  return {
    name: current.name.trim() ? current.name : 'Peri Agent',
    description: current.description,
    config: {
      instructions: '',
      provider: structuredClone(current.config.provider),
      metaHarness: {},
      mcpServers: {},
    },
  };
}

/** Stored agents are complete; this keeps one conversion point for forms and preview. */
export function draftFromAgent(agent: Agent): AgentDraft {
  return {
    name: agent.name,
    description: agent.description,
    config: {
      instructions: agent.config.instructions,
      provider: structuredClone(agent.config.provider),
      metaHarness: agent.config.metaHarness,
      mcpServers: agent.config.mcpServers,
    },
  };
}

export const EMPTY_AGENT_DRAFT: AgentDraft = {
  name: '',
  description: '',
  config: {
    instructions: '',
    provider: null,
    metaHarness: {},
    mcpServers: {},
  },
};
