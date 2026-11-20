import type { Context } from 'hono';
import { Hono } from 'hono';
import { buildAgentUpdate, buildAgentWrite, degradationForTools } from '../agents/mapping';
import { AgentUpdateParams, AgentWriteParams } from '../agents/schemas';
import { type AgentResourceBody, serializeAgent } from '../agents/serializer';
import { isHiddenAgentId } from '../agents/visibility';
import { platformError } from '../errors';
import { paginate, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import type { PlatformAgentRow } from '../repositories/agents';
import { parsePlatformJson } from '../request';

const NOT_FOUND_MESSAGE = 'Agent not found';
const HAS_SESSIONS_MESSAGE = 'Agent still has sessions; delete them first';

function respondWithAgent(c: Context, row: PlatformAgentRow, status: 200 | 201 = 200): Response {
  const body = serializeAgent(row);
  const degraded = degradationForTools(body.tools);
  if (degraded.length > 0) c.header('x-platform-degraded', degraded.join(','));
  return c.json(body, status);
}

function respondWithPage(
  c: Context,
  agents: readonly AgentResourceBody[],
  request: ReturnType<typeof parsePageRequest>,
): Response {
  const page = paginate(agents, request, (agent) => agent.id);
  const degraded = degradationForTools(page.data.flatMap((agent) => agent.tools));
  if (degraded.length > 0) c.header('x-platform-degraded', degraded.join(','));
  return c.json(page);
}

/** Routes for `/v1/agents` and the single-agent resource. */
export function createAgentRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const request = parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });
    const agents = repos.agents.listVisible(request.order).map(serializeAgent);
    return respondWithPage(c, agents, request);
  });

  routes.post('/', async (c) => {
    const params = await parsePlatformJson(c, AgentWriteParams);
    const row = repos.agents.create(buildAgentWrite(repos.agents.db, params));
    return respondWithAgent(c, row, 201);
  });

  routes.get('/:agent_id', (c) => {
    const id = c.req.param('agent_id');
    const row = isHiddenAgentId(id) ? null : repos.agents.get(id);
    if (row === null) return platformError(c, 'not_found', NOT_FOUND_MESSAGE, 404);
    return respondWithAgent(c, row);
  });

  routes.post('/:agent_id', async (c) => {
    const id = c.req.param('agent_id');
    const existing = isHiddenAgentId(id) ? null : repos.agents.get(id);
    if (existing === null) return platformError(c, 'not_found', NOT_FOUND_MESSAGE, 404);

    const params = await parsePlatformJson(c, AgentUpdateParams);
    const row = repos.agents.update(id, buildAgentUpdate(repos.agents.db, existing, params));
    if (row === null) return platformError(c, 'not_found', NOT_FOUND_MESSAGE, 404);
    return respondWithAgent(c, row);
  });

  routes.delete('/:agent_id', (c) => {
    const id = c.req.param('agent_id');
    const outcome = repos.agents.deleteIfUnreferenced(id);
    if (outcome === 'missing') return platformError(c, 'not_found', NOT_FOUND_MESSAGE, 404);
    if (outcome === 'referenced') return platformError(c, 'agent_has_sessions', HAS_SESSIONS_MESSAGE, 409);
    return c.json({ id, object: 'agent.deleted' as const, deleted: true });
  });

  return routes;
}
