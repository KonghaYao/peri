import { randomUUID } from 'node:crypto';
import { Server, createMcpHandler } from '@modelcontextprotocol/server';
import { z } from 'zod';
import { SandboxError, type McpBackendPort } from '../instance/types.js';

const objects = z.object({}).passthrough();
const methods = ['tools/call', 'tasks/get', 'tasks/update', 'tasks/cancel'] as const;

/**
 * MCP handler for one sandbox instance.
 *
 * Keeps the pinned `2026-07-28` protocol and rejects legacy negotiation; the
 * backend is fixed for the instance lifetime.
 */
export function createSandboxMcpHandler(options: {
  sandboxId: string;
  backend: McpBackendPort;
}): ReturnType<typeof createMcpHandler> {
  const { sandboxId, backend } = options;
  return createMcpHandler(
    () => {
      const server = new Server(
        { name: 'peri-sandbox', version: '0.1.0' },
        { capabilities: { tools: {}, extensions: { 'io.modelcontextprotocol/tasks': {} } } },
      );
      server.setRequestHandler('tools/list', async () => backend.listTools() as never);
      for (const method of methods) {
        server.setRequestHandler(method, { params: objects, result: objects }, async (params, context) => {
          const correlationId = randomUUID(),
            begun = Date.now();
          let outcome = 'error';
          try {
            const owner = context.mcpReq._meta?.['peri/taskOwner'];
            if (
              owner !== undefined &&
              (typeof owner !== 'string' || !owner.trim() || Buffer.byteLength(owner, 'utf8') > 512)
            )
              throw new SandboxError('invalid_options', 'invalid peri/taskOwner');
            const envelope = context.mcpReq.envelope as Record<string, unknown> | undefined;
            const capabilities = envelope?.['io.modelcontextprotocol/clientCapabilities'] as
              | { extensions?: Record<string, unknown> }
              | undefined;
            const arguments_ = params as Record<string, unknown>;
            if (
              (method !== 'tools/call' || arguments_.name === 'Bash') &&
              !capabilities?.extensions?.['io.modelcontextprotocol/tasks']
            )
              throw new SandboxError('invalid_options', 'client must declare Tasks capability');
            const token = context.mcpReq._meta?.progressToken as number | string | undefined;
            const result = await backend.forward(
              method,
              arguments_,
              owner as string | undefined,
              context.mcpReq.signal,
              token === undefined
                ? undefined
                : (progress) => {
                    void context.mcpReq
                      .notify({ method: 'notifications/progress', params: { ...progress, progressToken: token } })
                      .catch(() => {});
                  },
            );
            outcome = result.resultType === 'task' ? 'task' : 'complete';
            return result;
          } catch (error) {
            outcome = error instanceof SandboxError ? error.code : 'interrupted';
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
        });
      }
      return server;
    },
    { legacy: 'reject' },
  );
}
