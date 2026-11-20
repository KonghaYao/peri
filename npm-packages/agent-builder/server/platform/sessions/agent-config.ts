import { AgentConfig, type AgentDraft, type AgentProvider } from '@shared/agent';
import { parseStoredConfig } from '../../agents/repository';
import { SESSION_INLINE_AGENT_ID } from '../../db/migrate';
import { AppError } from '../../errors';
import type { ProviderService } from '../../providers/service';
import { parseJsonColumn } from '../json';
import type { PlatformAgentRow, PlatformAgentsRepository } from '../repositories/agents';
import { type AgentResourceWire, readJsonColumn, type SessionAgentSnapshot } from './serialize';

/** Documented inline agent fields; `model` is required when no `agent_id` is supplied. */
export interface SessionAgentInput {
  readonly model?: string | undefined;
  readonly instructions?: string | null | undefined;
  readonly reasoning?: unknown;
  readonly text?: unknown;
  readonly service_tier?: unknown;
  readonly tools?: readonly unknown[] | null | undefined;
  readonly multi_agent?: unknown;
  readonly compaction?: unknown;
  readonly experimental?: Record<string, unknown> | undefined;
}

export interface ResolvedSessionAgent {
  readonly agentId: string;
  readonly snapshot: SessionAgentSnapshot;
}

export interface ResolveSessionAgentOptions {
  readonly agents: PlatformAgentsRepository;
  readonly providers: ProviderService;
  readonly agentId?: string | undefined;
  readonly agent?: SessionAgentInput | undefined;
}

/**
 * Resolves the effective agent for a session: the saved agent row (or the synthetic inline row)
 * overlaid with the request's inline fields. The result is both the response snapshot and the
 * runtime draft, so a restart replays exactly the same configuration.
 */
export function resolveSessionAgent(options: ResolveSessionAgentOptions): ResolvedSessionAgent {
  const { agents, providers, agentId, agent } = options;
  const id = agentId ?? SESSION_INLINE_AGENT_ID;
  const base = agents.get(id);
  if (base === null) throw new AppError('not_found', 'Agent not found');

  const config = readAgentConfig(base, agents);
  const model = agent?.model ?? base.model ?? config.provider?.model ?? '';
  if (agentId === undefined && agent?.model === undefined) {
    throw new AppError('invalid_input', 'An inline agent requires model or agent_id');
  }

  const draft: AgentDraft = {
    name: base.name,
    description: base.description,
    config: {
      instructions: agent?.instructions ?? config.instructions,
      provider: resolveProvider(config.provider, model, providers),
      metaHarness: { ...config.metaHarness },
      mcpServers: { ...config.mcpServers },
    },
  };

  return {
    agentId: id,
    snapshot: { agent: agentResource(base, id, model, agent, config.instructions), draft },
  };
}

function readAgentConfig(base: PlatformAgentRow, agents: PlatformAgentsRepository): AgentDraft['config'] {
  // `agents.config` stores only the model for reference providers; `provider_id` is authoritative.
  // The provider column is not projected by the platform repository, so read it by key here.
  const providerId =
    agents.db
      .query<{ provider_id: string | null }, [string]>('SELECT provider_id FROM agents WHERE id = ?')
      .get(base.id)?.provider_id ?? null;
  try {
    return parseStoredConfig(base.id, base.config, providerId);
  } catch {
    return AgentConfig.parse({});
  }
}

/**
 * Applies update overrides to the stored snapshot. The draft and the response snapshot move
 * together so a later send freezes exactly what the resource reports.
 */
export function patchSessionAgent(
  snapshot: SessionAgentSnapshot,
  patch: SessionAgentInput,
): SessionAgentSnapshot {
  const model = patch.model ?? snapshot.agent.model;
  const provider = snapshot.draft.config.provider;
  const nextProvider =
    provider === null
      ? null
      : ({ ...provider, model: model.length > 0 ? model : provider.model } as AgentProvider);
  return {
    agent: {
      ...snapshot.agent,
      model,
      reasoning: pick(patch.reasoning, snapshot.agent.reasoning),
      text: pick(patch.text, snapshot.agent.text),
      service_tier: pick(patch.service_tier, snapshot.agent.service_tier),
      instructions: patch.instructions === undefined ? snapshot.agent.instructions : patch.instructions,
      tools: patch.tools === undefined ? snapshot.agent.tools : (patch.tools ?? []),
      multi_agent: pick(patch.multi_agent, snapshot.agent.multi_agent),
      compaction: pick(patch.compaction, snapshot.agent.compaction),
      experimental: mergeExperimental(snapshot.agent.experimental, patch.experimental),
    },
    draft: {
      ...snapshot.draft,
      config: {
        ...snapshot.draft.config,
        instructions: patch.instructions ?? snapshot.draft.config.instructions,
        provider: nextProvider,
      },
    },
  };
}

function resolveProvider(
  provider: AgentProvider | null,
  model: string,
  providers: ProviderService,
): AgentProvider | null {
  if (provider !== null) {
    const nextModel = model.length > 0 ? model : provider.model;
    return { ...provider, model: nextModel } as AgentProvider;
  }
  // The `/v1` agent contract has no provider field, so neither a saved agent nor an inline
  // agent can name one. Only an unambiguous deployment (exactly one configured Provider)
  // resolves implicitly; zero or several must fail visibly instead of picking one.
  const configured = providers.list();
  if (configured.length !== 1) {
    throw new AppError(
      'invalid_input',
      `Sessions require exactly one configured Provider; found ${configured.length}`,
    );
  }
  const only = configured[0];
  if (only === undefined) return null;
  return { kind: 'reference', providerId: only.id, model };
}

function agentResource(
  base: PlatformAgentRow,
  id: string,
  model: string,
  agent: SessionAgentInput | undefined,
  baseInstructions: string,
): AgentResourceWire {
  return {
    id,
    name: base.name,
    model,
    reasoning: pick(agent?.reasoning, readJsonColumn(base.reasoning)),
    text: pick(agent?.text, readJsonColumn(base.text_config)),
    service_tier: pick(agent?.service_tier, readJsonColumn(base.service_tier)),
    instructions: agent?.instructions === undefined ? baseInstructions : agent.instructions,
    tools: agent?.tools ?? parseJsonColumn<unknown[]>(base.tools, []),
    multi_agent: pick(agent?.multi_agent, readJsonColumn(base.multi_agent)),
    compaction: pick(agent?.compaction, readJsonColumn(base.compaction)),
    experimental: mergeExperimental(readJsonColumn(base.experimental), agent?.experimental),
  };
}

function pick(next: unknown, fallback: unknown): unknown {
  return next === undefined ? fallback : next;
}

/** The documented merge rule: experimental settings merge by key, everything else replaces. */
function mergeExperimental(base: unknown, override: Record<string, unknown> | undefined): unknown {
  const source =
    typeof base === 'object' && base !== null && !Array.isArray(base)
      ? (base as Record<string, unknown>)
      : {};
  if (override === undefined) return base === null ? null : source;
  return { ...source, ...override };
}
