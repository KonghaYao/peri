import { z } from 'zod';
import type { ResourceKind } from '../contracts/client.js';

export const RESOURCE_TOOL_NAMES = Object.freeze({
  get: 'resource_get',
} as const);

const RESOURCE_KINDS = [
  'agent_config',
  'provider_list',
  'mcp_plugin_config',
  'model_settings',
] as const satisfies readonly ResourceKind[];

const resourceKind = z.enum(RESOURCE_KINDS);

export const resourceGetInput = z.object({
  resourceId: z
    .string()
    .min(1)
    .max(200)
    .describe(
      'Resource identity. The caller scope is always taken from authentication, never from arguments.',
    ),
});

export const resourceGetOutput = z.object({
  kind: resourceKind,
  revision: z.string().describe('Opaque version; compare for changes only.'),
  value: z.unknown().describe('Server-side masked JSON value; secrets are never returned.'),
  observedAt: z.string(),
});

export type ResourceGetArgs = z.infer<typeof resourceGetInput>;
