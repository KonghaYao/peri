import { createApiErrorBody } from '@shared/api';
import type { MiddlewareHandler } from 'hono';

function hostnameOf(host: string): string {
  const trimmed = host.trim().toLowerCase();
  if (trimmed.startsWith('[')) {
    const end = trimmed.indexOf(']');
    return end === -1 ? '' : trimmed.slice(1, end);
  }
  return trimmed.split(':')[0] ?? '';
}

function isLoopback(hostname: string): boolean {
  return hostname === '127.0.0.1' || hostname === 'localhost' || hostname === '::1';
}

/**
 * Blocks DNS rebinding and cross-site browser requests to the local API.
 *
 * The API has high-privilege tools and holds provider/MCP/store credentials server-side, so any
 * web origin that can POST here is a security boundary. Same-origin localhost and the explicitly
 * configured dev origin are allowed; everything else fails before routing.
 */
export function createOriginGuard(allowedOrigins: readonly string[]): MiddlewareHandler {
  const allowed = new Set(allowedOrigins.map((origin) => origin.replace(/\/$/, '')));
  return async (c, next) => {
    const host = c.req.header('host');
    if (host !== undefined && host.length > 0 && !isLoopback(hostnameOf(host))) {
      return c.json(createApiErrorBody('invalid_identity', 'Host is not allowed'), 403);
    }

    const fetchSite = c.req.header('sec-fetch-site')?.toLowerCase();
    if (fetchSite === 'cross-site') {
      return c.json(createApiErrorBody('invalid_identity', 'Cross-site request is not allowed'), 403);
    }

    const origin = c.req.header('origin');
    if (origin !== undefined && origin.length > 0) {
      const normalized = origin.replace(/\/$/, '');
      let hostname = '';
      try {
        hostname = new URL(normalized).hostname;
      } catch {
        hostname = '';
      }
      if (!allowed.has(normalized) || !isLoopback(hostname)) {
        return c.json(createApiErrorBody('invalid_identity', 'Request origin is not allowed'), 403);
      }
    }

    return next();
  };
}
