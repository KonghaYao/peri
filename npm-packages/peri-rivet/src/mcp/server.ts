import { type AuthInfo, type CallToolResult, McpServer } from '@modelcontextprotocol/server';
import type { ResourceSnapshot } from '../contracts/client.js';
import { ActorError, isActorError } from '../errors.js';
import { parseResourceId, parseResourceScope, type ResourceScope } from '../identity.js';
import type { JsonObject } from '../json.js';
import type { ResourceQueries } from '../resource/queries.js';
import { ACTOR_VERSION } from '../version.js';
import { RESOURCE_TOOL_NAMES, resourceGetInput, resourceGetOutput } from './schemas.js';

/**
 * Private `AuthInfo.extra` key used to move the authenticated scope from the transport edge into
 * the tool handler. MCP treats `authInfo` as pass-through data, so a request body cannot set it.
 */
export const SCOPE_AUTH_EXTRA_KEY = 'peri.actor.resourceScope';

export interface CreateResourceMcpServerOptions {
  readonly queries: ResourceQueries;
}

/**
 * Creates a stateless MCP server that exposes resource reads only.
 *
 * The scope is resolved per request from `authInfo`; the tool schema deliberately has no scope
 * argument so a caller cannot address another tenant.
 */
export function createResourceMcpServer(options: CreateResourceMcpServerOptions): McpServer {
  const server = new McpServer({ name: 'peri-resource', version: ACTOR_VERSION });

  server.registerTool(
    RESOURCE_TOOL_NAMES.get,
    {
      title: 'Get resource',
      description:
        'Read the current snapshot of a resource in the authenticated scope. Returns a tool error when the resource does not exist in that scope.',
      inputSchema: resourceGetInput,
      outputSchema: resourceGetOutput,
      annotations: { readOnlyHint: true, openWorldHint: false },
    },
    async (args, context) =>
      runTool(async () => {
        const scope = requireScope(context.http?.authInfo);
        const snapshot = await options.queries.get(scope, parseResourceId(args.resourceId));
        if (snapshot === null) {
          throw new ActorError('resource_not_found', 'resource not found', {
            resourceId: args.resourceId,
          });
        }
        return snapshotRecord(snapshot);
      }),
  );

  return server;
}

function requireScope(authInfo: AuthInfo | undefined): ResourceScope {
  const value = authInfo?.extra?.[SCOPE_AUTH_EXTRA_KEY];
  if (typeof value !== 'string') {
    throw new ActorError(
      'mcp_scope_required',
      'MCP request reached the server without an authenticated scope',
      {
        reason: 'missing_auth_scope',
      },
    );
  }
  return parseResourceScope(value);
}

async function runTool(execute: () => Promise<JsonObject>): Promise<CallToolResult> {
  try {
    return okResult(await execute());
  } catch (error) {
    if (isActorError(error)) {
      return errorResult(error);
    }
    throw error;
  }
}

function okResult(output: JsonObject): CallToolResult {
  return {
    content: [{ type: 'text', text: JSON.stringify(output) }],
    structuredContent: output,
  };
}

function errorResult(error: ActorError): CallToolResult {
  return {
    content: [{ type: 'text', text: JSON.stringify({ error: error.toJSON() }) }],
    isError: true,
  };
}

function snapshotRecord(snapshot: ResourceSnapshot): JsonObject {
  return {
    kind: snapshot.kind,
    revision: snapshot.revision,
    value: snapshot.value,
    observedAt: snapshot.observedAt,
  };
}
