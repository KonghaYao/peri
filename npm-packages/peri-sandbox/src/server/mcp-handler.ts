import { randomUUID } from 'node:crypto';
import { Server, createMcpHandler } from '@modelcontextprotocol/server';
import { ProtocolError, ProtocolErrorCode } from '@modelcontextprotocol/client';
import { z } from 'zod';
import { SandboxError, type McpBackendPort } from '../instance/types.js';

const objects = z.object({}).passthrough();
const results = z.record(z.string(), z.unknown());
const taskMethods = ['tasks/get', 'tasks/update', 'tasks/cancel'] as const;

/**
 * MCP handler for one sandbox instance.
 *
 * Keeps the pinned `2026-07-28` protocol and rejects legacy negotiation; the
 * backend is fixed for the instance lifetime. Capabilities, identity,
 * instructions and every request/notification are mirrored from the inner
 * backend without filtering or rewriting.
 */
export function createSandboxMcpHandler(options: {
  sandboxId: string;
  backend: McpBackendPort;
}): ReturnType<typeof createMcpHandler> {
  const { sandboxId, backend } = options;
  return createMcpHandler(
    () => {
      const server = new Server(backend.serverInfo() ?? { name: 'peri-sandbox', version: '0.1.0' }, {
        capabilities: structuredClone(backend.serverCapabilities() ?? {}),
        instructions: backend.instructions(),
        supportedProtocolVersions: ['2026-07-28'],
      });
      // The mirrored `logging` capability makes the SDK answer
      // `logging/setLevel` locally; remove that handler so the request reaches
      // the inner server like every other declared capability.
      server.removeRequestHandler('logging/setLevel');
      const forward = async (
        method: string,
        params: Record<string, unknown>,
        context: Parameters<NonNullable<typeof server.fallbackRequestHandler>>[1],
      ): Promise<Record<string, unknown>> => {
        const correlationId = randomUUID();
        const begun = Date.now();
        let outcome = 'error';
        try {
          const token = context.mcpReq._meta?.progressToken as number | string | undefined;
          const result = await backend.request(method, params, {
            signal: context.mcpReq.signal,
            requestId: context.mcpReq.id,
            onProgress:
              token === undefined
                ? undefined
                : (progress) => {
                    void context.mcpReq
                      .notify({
                        method: 'notifications/progress',
                        params: { ...progress, progressToken: token },
                      })
                      .catch(() => {});
                  },
          });
          outcome = result.resultType === 'task' ? 'task' : 'complete';
          return result;
        } catch (error) {
          if (error instanceof SandboxError) {
            outcome = error.code;
            throw new ProtocolError(
              error.code === 'invalid_options' ? ProtocolErrorCode.InvalidParams : ProtocolErrorCode.InternalError,
              error.message,
              { sandbox: error.code },
            );
          }
          outcome =
            typeof (error as { code?: unknown })?.code === 'number'
              ? String((error as { code: number }).code)
              : 'interrupted';
          throw error;
        } finally {
          console.info(
            JSON.stringify({
              event: 'mcp_request',
              sandboxId,
              runId: backend.runId,
              correlationId,
              method,
              durationMs: Date.now() - begun,
              outcome,
            }),
          );
        }
      };
      // Tasks methods are reused spec names that need an explicit custom
      // schema for the SDK era gate. The payload is still forwarded verbatim.
      for (const method of taskMethods) {
        server.setRequestHandler(method, { params: objects, result: results }, (params, context) =>
          forward(method, params as Record<string, unknown>, context),
        );
      }
      server.fallbackRequestHandler = (request, context) =>
        forward(request.method, (request.params ?? {}) as Record<string, unknown>, context) as never;
      server.setNotificationHandler('notifications/cancelled', (notification) => {
        const requestId = notification.params?.requestId;
        if (requestId !== undefined) backend.cancel(requestId);
      });
      server.fallbackNotificationHandler = async (notification) => {
        try {
          await backend.notify({
            method: notification.method,
            params: notification.params as Record<string, unknown> | undefined,
          });
        } catch (error) {
          console.warn('sandbox notification forward failed', error);
        }
      };
      return server;
    },
    { legacy: 'reject', keepAliveMs: 10_000, maxRequestBodySize: 8 * 1024 * 1024 },
  );
}
