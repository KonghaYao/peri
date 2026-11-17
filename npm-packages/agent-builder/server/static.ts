import { existsSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createApiErrorBody } from '@shared/api';
import type { Context, Hono } from 'hono';

/** Dev runs this file from `server/`; the packaged server is bundled under `dist/server/`. */
function candidates(explicit: string | undefined): string[] {
  // An explicit root is authoritative: a typo must not silently serve the package's own dist.
  if (explicit !== undefined) return [explicit];
  const fromModule = path.dirname(fileURLToPath(import.meta.url));
  return [path.resolve(fromModule, '..', 'dist'), path.resolve(fromModule, '..')];
}

export function findWebRoot(explicit: string | undefined): string | null {
  for (const candidate of candidates(explicit)) {
    if (existsSync(path.join(candidate, 'index.html'))) return candidate;
  }
  return null;
}

function jsonNotFound(c: Context): Response {
  return c.json(createApiErrorBody('not_found', 'Route not found'), 404);
}

/**
 * Serves the built SPA from the package itself. Paths are resolved lexically and must stay below
 * the web root; `/api/*` never falls through to an HTML shell.
 */
export function mountStatic(app: Hono, root: string, assetVersion: string): void {
  app.get('*', async (c) => {
    const pathname = c.req.path;
    if (pathname === '/api' || pathname.startsWith('/api/')) return jsonNotFound(c);

    let decoded: string;
    try {
      decoded = decodeURIComponent(pathname);
    } catch {
      return c.json(createApiErrorBody('invalid_input', 'Malformed path'), 400);
    }
    const relative = decoded.replace(/^\/+/, '');
    const resolved = path.resolve(root, relative);
    if (resolved !== root && !resolved.startsWith(`${root}${path.sep}`)) {
      return c.json(createApiErrorBody('invalid_input', 'Path escapes the web root'), 400);
    }

    const wantsHtml = (c.req.header('accept') ?? '').includes('text/html');
    const lookup = decoded.endsWith('/') || relative.length === 0 ? path.join(root, 'index.html') : resolved;
    const file = existsSync(lookup) && statSync(lookup).isFile() ? lookup : null;

    if (file === null) {
      // Client-side routes fall back to the shell; missing assets stay a real 404.
      if (!wantsHtml || path.extname(decoded).length > 0) return jsonNotFound(c);
      return serve(path.join(root, 'index.html'), false, assetVersion);
    }
    return serve(file, decoded.startsWith('/assets/'), assetVersion);
  });
}

function serve(file: string, immutable: boolean, assetVersion: string): Response {
  const headers = new Headers({
    'x-content-type-options': 'nosniff',
    'referrer-policy': 'no-referrer',
    'x-frame-options': 'DENY',
    'cache-control': immutable ? 'public, max-age=31536000, immutable' : 'no-store',
  });
  if (!immutable) headers.set('etag', `"${assetVersion}"`);
  return new Response(Bun.file(file), { headers });
}
