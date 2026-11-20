import type { Database } from 'bun:sqlite';
import { Hono } from 'hono';
import { platformError, platformErrorFromUnknown } from '../errors';
import { createPlatformRepositories } from '../repositories';
import { requirePlatformAuthorization } from '../request';
import { createAgentRoutes } from './agents';
import { createAutomationInvocationRoutes } from './automation-invocations';
import { createAutomationRoutes } from './automations';
import { createEnvironmentFileRoutes } from './environment-files';
import { createEnvironmentTemplateRoutes } from './environment-templates';
import { createEnvironmentRoutes } from './environments';
import { createSessionArtifactRoutes } from './session-artifacts';
import { createSessionEventRoutes } from './session-events';
import { createSessionItemRoutes } from './session-items';
import { createSessionSubagentRoutes } from './session-subagents';
import { createSessionTraceRoutes } from './session-traces';
import { createSessionTurnRoutes } from './session-turns';
import { createSessionRoutes } from './sessions';
import { createVaultCredentialRoutes } from './vault-credentials';
import { createVaultRoutes } from './vaults';

export interface PlatformRouteDependencies {
  readonly db: Database;
}

/**
 * Mounts the platform compatibility surface below `/v1`.
 *
 * Every documented path is registered with its documented method and path parameters.
 * Handlers return the canonical `not_implemented` error until their resource family lands.
 * Auth is presence-only: any bearer token is accepted and never recorded.
 */
export function createPlatformRoutes(deps: PlatformRouteDependencies): Hono {
  const routes = new Hono();
  const repos = createPlatformRepositories(deps.db);

  routes.use('*', requirePlatformAuthorization());

  // Specific prefixes must be mounted before `/agents/:agent_id`, which would otherwise
  // capture `/agents/sessions` and `/agents/environments` as an agent ID.
  routes.route('/agents/automations/:automation_id/invocations', createAutomationInvocationRoutes(repos));
  routes.route('/agents/automations', createAutomationRoutes(repos));
  routes.route('/agents/sessions/:session_id/subagents', createSessionSubagentRoutes(repos));
  routes.route('/agents/sessions/:session_id/events', createSessionEventRoutes(repos));
  routes.route('/agents/sessions/:session_id/items', createSessionItemRoutes(repos));
  routes.route('/agents/sessions/:session_id/turns', createSessionTurnRoutes(repos));
  routes.route('/agents/sessions/:session_id/artifacts', createSessionArtifactRoutes(repos));
  routes.route('/agents/sessions/:session_id/traces', createSessionTraceRoutes(repos));
  routes.route('/agents/sessions', createSessionRoutes(repos));
  routes.route('/agents/environments/templates', createEnvironmentTemplateRoutes(repos));
  routes.route('/agents/environments/:environment_id/files', createEnvironmentFileRoutes(repos));
  routes.route('/agents/environments', createEnvironmentRoutes(repos));
  routes.route('/agents', createAgentRoutes(repos));
  routes.route('/vaults/:vault_id/credentials', createVaultCredentialRoutes(repos));
  routes.route('/vaults', createVaultRoutes(repos));

  routes.all('*', (c) => platformError(c, 'not_found', 'Route not found', 404));
  routes.notFound((c) => platformError(c, 'not_found', 'Route not found', 404));
  routes.onError((error, c) => platformErrorFromUnknown(error, c));

  return routes;
}
