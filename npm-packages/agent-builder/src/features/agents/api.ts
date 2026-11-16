import {
  type Agent,
  AgentCapabilities,
  type AgentDraft,
  AgentListResponse,
  AgentPreviewResponse,
  AgentResponse,
  type AgentSummary,
} from '@shared/agent';
import { useQuery } from '@tanstack/react-query';
import { apiRequest, apiRequestVoid, jsonInit } from '@/lib/api-client';
import { draftFromAgent } from './draft';

export async function listAgents(): Promise<AgentSummary[]> {
  const body = await apiRequest('/api/agents', undefined, AgentListResponse);
  return body.agents;
}

export async function getAgent(agentId: string): Promise<Agent> {
  const body = await apiRequest(`/api/agents/${agentId}`, undefined, AgentResponse);
  return body.agent;
}

export async function createAgent(draft: AgentDraft): Promise<Agent> {
  const body = await apiRequest('/api/agents', jsonInit('POST', draft), AgentResponse);
  return body.agent;
}

export async function updateAgent(agentId: string, draft: AgentDraft, revision: number): Promise<Agent> {
  const body = await apiRequest(
    `/api/agents/${agentId}`,
    {
      ...jsonInit('PUT', draft),
      headers: { 'content-type': 'application/json', 'If-Match': String(revision) },
    },
    AgentResponse,
  );
  return body.agent;
}

export async function deleteAgent(agentId: string, revision: number): Promise<void> {
  return apiRequestVoid(`/api/agents/${agentId}`, {
    method: 'DELETE',
    headers: { 'If-Match': String(revision) },
  });
}

export async function previewAgent(draft: AgentDraft) {
  return apiRequest('/api/agents/preview', jsonInit('POST', draft), AgentPreviewResponse);
}

export async function fetchCapabilities() {
  return apiRequest('/api/capabilities', undefined, AgentCapabilities);
}

export function useAgentList() {
  return useQuery({ queryKey: ['agents'], queryFn: listAgents });
}

export function useAgent(agentId: string | undefined) {
  return useQuery({
    queryKey: ['agent', agentId],
    queryFn: () => {
      if (!agentId) throw new Error('agentId is required');
      return getAgent(agentId);
    },
    enabled: agentId !== undefined,
  });
}

/**
 * The runtime status can change while the process runs (Store outage/recovery), so it must not be
 * cached forever. A short TTL lets a recovery clear the notice without a reload.
 */
export function useCapabilities() {
  return useQuery({
    queryKey: ['capabilities'],
    queryFn: fetchCapabilities,
    staleTime: 5_000,
  });
}

/** Preview is derived from persisted state, so it re-runs only when the stored agent changes. */
export function useAgentPreview(agent: Agent | undefined) {
  return useQuery({
    queryKey: ['agent', agent?.id ?? 'none', 'preview', agent?.revision ?? 0],
    queryFn: () => {
      if (!agent) throw new Error('agent is required');
      return previewAgent(draftFromAgent(agent));
    },
    enabled: agent !== undefined,
    retry: false,
  });
}
