import type { AgentListResponse, AgentPreviewResponse, AgentResponse } from '@shared/agent';
import type { ChatRuntimeStatus, SandboxStatus } from '@shared/chat';
import { Hono } from 'hono';
import type { ServerEnv } from '../env';
import { AppError, readJsonBody } from '../errors';
import { readRevision } from '../providers/routes';
import type { ProviderService } from '../providers/service';
import { createPreviewResponse } from './open-target';
import { AgentHasChatsError, type AgentsRepository } from './repository';
import { parseAgentDraft } from './schema';

export type RuntimeStatusProvider = () => ChatRuntimeStatus;

const NOT_CONFIGURED: RuntimeStatusProvider = () => ({ mode: 'not_configured', reasons: [] });
const SANDBOX_DISABLED: SandboxStatus = { mode: 'disabled', reasons: [] };

export function createAgentsRoutes(
  repository: AgentsRepository,
  env: ServerEnv,
  getRuntime: RuntimeStatusProvider = NOT_CONFIGURED,
  sandbox: SandboxStatus = SANDBOX_DISABLED,
  providers?: ProviderService,
): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const body: AgentListResponse = { agents: repository.list() };
    return c.json(body);
  });

  routes.post('/', async (c) => {
    const draft = parseAgentDraft(await readJsonBody(c));
    const body: AgentResponse = { agent: repository.create(draft) };
    return c.json(body, 201);
  });

  /** Compose-only: nothing is written, and every credential value is redacted. */
  routes.post('/preview', async (c) => {
    const draft = parseAgentDraft(await readJsonBody(c));
    const body: AgentPreviewResponse = createPreviewResponse(env, draft, getRuntime(), sandbox, providers);
    return c.json(body);
  });

  routes.get('/:agentId', (c) => {
    const agent = repository.get(c.req.param('agentId'));
    if (!agent) throw new AppError('not_found', 'Agent not found');
    const body: AgentResponse = { agent };
    return c.json(body);
  });

  routes.put('/:agentId', async (c) => {
    const revision = readRevision(c.req.header('If-Match'));
    const draft = parseAgentDraft(await readJsonBody(c));
    const agent = repository.update(c.req.param('agentId'), draft, revision);
    if (!agent) throw new AppError('not_found', 'Agent not found');
    const body: AgentResponse = { agent };
    return c.json(body);
  });

  routes.delete('/:agentId', (c) => {
    try {
      if (!repository.delete(c.req.param('agentId'), readRevision(c.req.header('If-Match')))) {
        throw new AppError('not_found', 'Agent not found');
      }
    } catch (error) {
      if (error instanceof AgentHasChatsError) {
        throw new AppError('agent_has_chats', 'Agent still has chats; delete the chats first');
      }
      throw error;
    }
    return c.body(null, 204);
  });

  return routes;
}
