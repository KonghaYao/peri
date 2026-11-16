import {
  type AuthInfo,
  type CreateMcpHandlerOptions,
  createMcpHandler,
  type McpHttpHandler,
} from '@modelcontextprotocol/server';
import { ActorError } from '../errors.js';
import { parseResourceScope, type ResourceScope } from '../identity.js';
import type { ResourceQueries } from '../resource/queries.js';
import { createResourceMcpServer, SCOPE_AUTH_EXTRA_KEY } from './server.js';

export type ResourceScopeResolver = (
  request: Request,
) => ResourceScope | null | Promise<ResourceScope | null>;

export interface ResourceMcpHttpOptions {
  readonly queries: ResourceQueries;
  /** Authenticates the transport request. Returning `null` produces a 401 response. */
  readonly resolveScope: ResourceScopeResolver;
  /** `stateless` keeps 2025-era clients working; use `reject` for a modern-only endpoint. */
  readonly legacy?: 'stateless' | 'reject';
  readonly keepAliveMs?: number;
  readonly maxRequestBodySize?: number;
  readonly onError?: (error: Error) => void;
}

export interface ResourceMcpHttpHandler {
  fetch(request: Request): Promise<Response>;
  close(): Promise<void>;
}

/**
 * Wraps a standard Streamable HTTP MCP handler with scope authentication.
 *
 * The returned handler is fetch-shaped so it can run on Node, Bun, Workers or any host that
 * exposes the web `Request`/`Response` pair. MCP sessions are deliberately not tied to a resource:
 * every request reconstructs a stateless server over the same queries.
 */
export function createResourceMcpHttpHandler(options: ResourceMcpHttpOptions): ResourceMcpHttpHandler {
  const handlerOptions: CreateMcpHandlerOptions = {
    legacy: options.legacy ?? 'stateless',
    ...(options.keepAliveMs === undefined ? {} : { keepAliveMs: options.keepAliveMs }),
    ...(options.maxRequestBodySize === undefined ? {} : { maxRequestBodySize: options.maxRequestBodySize }),
    ...(options.onError === undefined ? {} : { onerror: options.onError }),
  };

  const handler: McpHttpHandler = createMcpHandler((context) => {
    const scopeValue = context.authInfo?.extra?.[SCOPE_AUTH_EXTRA_KEY];
    if (typeof scopeValue !== 'string') {
      throw new ActorError(
        'mcp_scope_required',
        'MCP request reached the server without an authenticated scope',
        {
          reason: 'missing_auth_scope',
        },
      );
    }
    return createResourceMcpServer({ queries: options.queries });
  }, handlerOptions);

  return {
    async fetch(request) {
      const resolved = await options.resolveScope(request);
      if (resolved === null) {
        return unauthorizedResponse();
      }
      const scope = parseResourceScope(resolved);
      const authInfo: AuthInfo = {
        token: '',
        clientId: 'peri-rivet',
        scopes: [],
        extra: { [SCOPE_AUTH_EXTRA_KEY]: scope },
      };
      return handler.fetch(request, { authInfo });
    },
    close: () => handler.close(),
  };
}

function unauthorizedResponse(): Response {
  return new Response(
    JSON.stringify({
      error: {
        code: 'mcp_scope_required',
        message: 'The request did not resolve to an authenticated resource scope.',
      },
    }),
    {
      status: 401,
      headers: { 'content-type': 'application/json; charset=utf-8' },
    },
  );
}
