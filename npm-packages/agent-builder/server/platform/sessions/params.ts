import { z } from 'zod';

const MAX_ID_LENGTH = 1_048_576;

export const SessionMetadataParams = z
  .record(z.string().min(1).max(64), z.string().max(512))
  .refine((value) => Object.keys(value).length <= 16, 'metadata accepts at most 16 pairs');

const PromptCacheOptions = z.strictObject({ ttl: z.string().min(1).max(64).optional() });

/**
 * The documented environment union. Hosted shapes are accepted at the wire boundary because the
 * spec allows them; the service answers `501` for a hosting type it cannot provision instead of
 * rewriting the request to a local environment.
 */
const HostedEnvironmentParams = z.strictObject({
  type: z.literal('openai_hosted'),
  packages: z.unknown().optional(),
  setup_commands: z.unknown().optional(),
  network: z.unknown().optional(),
  desktop: z.unknown().optional(),
  env: z.unknown().optional(),
  capability_directories: z.unknown().optional(),
  skills: z.unknown().optional(),
  plugins: z.unknown().optional(),
  files: z.unknown().optional(),
  environment_id: z.unknown().optional(),
  environment_template_id: z.unknown().optional(),
  container_size: z.unknown().optional(),
});

const EnvironmentParams = z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('none') }),
  z.strictObject({
    type: z.literal('self_hosted'),
    workspace_directory: z.string().max(MAX_ID_LENGTH),
    capability_directories: z.array(z.string().max(MAX_ID_LENGTH)).max(16_384).nullish(),
  }),
  HostedEnvironmentParams,
]);

const SessionAgentParams = z.strictObject({
  model: z.string().max(MAX_ID_LENGTH).optional(),
  instructions: z.string().max(MAX_ID_LENGTH).nullable().optional(),
  reasoning: z.unknown().optional(),
  text: z.unknown().optional(),
  service_tier: z.unknown().optional(),
  tools: z.array(z.unknown()).max(16_384).nullable().optional(),
  multi_agent: z.unknown().optional(),
  compaction: z.unknown().optional(),
  experimental: z.record(z.string(), z.unknown()).optional(),
});

const InputContent = z.strictObject({
  type: z.string().min(1),
  text: z.string().optional(),
  image_url: z.string().optional(),
});

const InputMessage = z.strictObject({
  type: z.literal('message').optional(),
  role: z.literal('user'),
  content: z.array(InputContent).max(16_384),
});

const SessionInput = z.union([z.string().min(1).max(MAX_ID_LENGTH), z.array(InputMessage).max(16_384)]);

export type SessionInputParam = z.infer<typeof SessionInput>;

export const CreateSessionParams = z.strictObject({
  environment: EnvironmentParams,
  agent: SessionAgentParams.optional(),
  agent_id: z.string().max(64).optional(),
  vault_ids: z.array(z.string().max(MAX_ID_LENGTH)).max(16_384).nullable().optional(),
  input: SessionInput.nullable().optional(),
  metadata: SessionMetadataParams.nullable().optional(),
  prompt_cache_options: PromptCacheOptions.nullable().optional(),
  review: z.enum(['none', 'auto_review']).optional(),
  stream: z.boolean().optional(),
  stream_options: z.strictObject({ include_obfuscation: z.boolean().optional() }).optional(),
});
export type CreateSessionParams = z.infer<typeof CreateSessionParams>;

export const UpdateSessionParams = z.strictObject({
  prompt_cache_options: PromptCacheOptions.nullable().optional(),
  vault_ids: z.array(z.string().max(MAX_ID_LENGTH)).max(16_384).nullable().optional(),
  review: z.enum(['none', 'auto_review']).optional(),
  agent: SessionAgentParams.optional(),
  metadata: SessionMetadataParams.nullable().optional(),
});
export type UpdateSessionParams = z.infer<typeof UpdateSessionParams>;

const MessageInput = z.strictObject({
  type: z.literal('agent.session.input.message'),
  input: z.array(InputMessage).max(16_384),
});

const CancelInput = z.strictObject({ type: z.literal('agent.session.input.cancel') });

const ApprovalInput = z.strictObject({
  type: z.literal('agent.session.input.mcp_approval_request_result'),
  request_id: z.string().min(1).max(MAX_ID_LENGTH),
  response: z.strictObject({
    type: z.literal('mcp_tool_call'),
    decision: z.enum(['approve', 'deny', 'cancel']),
  }),
});

/** Every other documented input type is deliberately absent: the service answers 400 for it. */
export const SessionEventInput = z.discriminatedUnion('type', [MessageInput, CancelInput, ApprovalInput]);
export type SessionEventInput = z.infer<typeof SessionEventInput>;

export const CreateSessionEventsParams = z.strictObject({
  events: z.array(SessionEventInput).max(16_384),
});
export type CreateSessionEventsParams = z.infer<typeof CreateSessionEventsParams>;

/** Flattens `agent.session.input.message` content into the text the chat runtime accepts. */
export function inputText(input: SessionInputParam | null | undefined): string | null {
  if (input === null || input === undefined) return null;
  if (typeof input === 'string') return input;
  const parts: string[] = [];
  for (const message of input) {
    for (const content of message.content) {
      if (content.type === 'input_text' && typeof content.text === 'string') parts.push(content.text);
    }
  }
  return parts.join('\n');
}

export function messageInputText(input: z.infer<typeof MessageInput>): string {
  const parts: string[] = [];
  for (const message of input.input) {
    for (const content of message.content) {
      if (content.type === 'input_text' && typeof content.text === 'string') parts.push(content.text);
    }
  }
  return parts.join('\n');
}
