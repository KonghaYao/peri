import type { Context } from 'hono';
import { Hono } from 'hono';
import { platformError } from '../errors';
import { paginate, parsePageRequest } from '../pagination';
import type { PlatformRepositories } from '../repositories';
import type { PlatformArtifactRow } from '../repositories/artifacts';
import { sessionErrorResponse } from '../sessions/errors';
import { ArtifactContentError, readArtifactContent, serializeArtifact } from '../subagents/artifacts';

function sessionMissing(c: Context): Response {
  return platformError(c, 'not_found', 'Session not found', 404);
}

function artifactOf(
  repos: PlatformRepositories,
  sessionId: string,
  artifactId: string,
): PlatformArtifactRow | null {
  const row = repos.artifacts.get(artifactId);
  if (row === null || row.session_id !== sessionId) return null;
  return row;
}

/**
 * Routes for `/v1/agents/sessions/{session_id}/artifacts`.
 *
 * This deployment does not publish artifacts from a hosted environment, so the collection is
 * normally empty and single-item reads are 404. Rows that exist are still served from
 * `platform_artifacts`, including guarded content reads.
 */
export function createSessionArtifactRoutes(repos: PlatformRepositories): Hono {
  const routes = new Hono();

  routes.get('/', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    if (repos.sessions.get(sessionId) === null) return sessionMissing(c);

    const request = parsePageRequest({
      limit: c.req.query('limit'),
      order: c.req.query('order'),
      after: c.req.query('after'),
    });
    const environmentId = c.req.query('environment_id');
    const all = repos.artifacts.listBySession(sessionId).map(serializeArtifact);
    // Local rows are not bound to a hosted environment; a non-empty filter therefore matches none.
    const visible =
      environmentId === undefined || environmentId.length === 0
        ? all
        : all.filter((artifact) => artifact.environment_id === environmentId);
    const rows = request.order === 'asc' ? visible : [...visible].reverse();
    return c.json(paginate(rows, request, (artifact) => artifact.id));
  });

  routes.get('/:artifact_id', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    if (repos.sessions.get(sessionId) === null) return sessionMissing(c);
    const row = artifactOf(repos, sessionId, c.req.param('artifact_id') ?? '');
    if (row === null) return platformError(c, 'not_found', 'Artifact not found', 404);
    return c.json(serializeArtifact(row));
  });

  routes.delete('/:artifact_id', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    if (repos.sessions.get(sessionId) === null) return sessionMissing(c);
    const id = c.req.param('artifact_id') ?? '';
    if (artifactOf(repos, sessionId, id) === null || !repos.artifacts.delete(id)) {
      return platformError(c, 'not_found', 'Artifact not found', 404);
    }
    return c.json({ id, object: 'agent.session.artifact.deleted' as const, deleted: true });
  });

  routes.get('/:artifact_id/content', (c) => {
    const sessionId = c.req.param('session_id') ?? '';
    if (repos.sessions.get(sessionId) === null) return sessionMissing(c);
    const row = artifactOf(repos, sessionId, c.req.param('artifact_id') ?? '');
    if (row === null) return platformError(c, 'not_found', 'Artifact not found', 404);
    try {
      const content = readArtifactContent(row);
      return c.body(content.bytes, 200, { 'content-type': content.contentType });
    } catch (error) {
      if (error instanceof ArtifactContentError) {
        return platformError(c, error.code, error.message, error.status);
      }
      throw error;
    }
  });

  routes.onError(sessionErrorResponse);
  return routes;
}
