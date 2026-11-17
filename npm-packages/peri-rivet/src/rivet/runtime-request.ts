/**
 * Headers owned by the outer actor/gateway. A sandbox runtime must never see them: a custom
 * runtime port is an authorization boundary, not an internal trusted hop.
 */
const OUTER_CREDENTIAL_HEADERS = [
  'authorization',
  'cookie',
  'host',
  'proxy-authorization',
  'x-peri-sandbox-token',
  'x-rivet-token',
] as const;

/**
 * Builds the request forwarded to a custom sandbox runtime.
 *
 * Construct from URL + init, never `new Request(request, { headers })`: Bun 1.4.3 keeps the
 * source request's headers when the header override is an empty `Headers`, so stripping the only
 * credential still forwarded it. Copying every non-credential header keeps MCP streaming headers
 * (`accept`, `content-type`, `mcp-session-id`, `last-event-id`) while dropping outer credentials.
 */
export function projectRuntimeRequest(request: Request, signal: AbortSignal): Request {
  const headers = new Headers(request.headers);
  for (const name of OUTER_CREDENTIAL_HEADERS) {
    headers.delete(name);
  }
  const bodyless = request.method === 'GET' || request.method === 'HEAD';
  const init: RequestInit & { duplex?: 'half' } = {
    method: request.method,
    headers,
    signal,
    redirect: request.redirect,
    ...(bodyless || request.body === null ? {} : { body: request.body, duplex: 'half' }),
  };
  return new Request(request.url, init);
}
