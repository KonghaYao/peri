import type { JsonValue } from './json.js';

export const ACTOR_ERROR_CODES = [
  'invalid_identity',
  'invalid_json',
  'invalid_input',
  'invalid_limit',
  'activity_invariant',
  'runtime_closing',
  'runtime_closed',
  'resource_closed',
  'request_conflict',
  'binding_conflict',
  'resource_not_found',
  'unknown_provider',
  'storage_schema_unsupported',
  'storage_corrupt',
  'storage_closed',
  'resource_mismatch',
  'mcp_scope_required',
] as const;

export type ActorErrorCode = (typeof ACTOR_ERROR_CODES)[number];

export type ActorErrorDetails = Readonly<Record<string, JsonValue>>;

/**
 * The single error type crossing this package's public boundaries.
 *
 * A stable `code` lets MCP and HTTP adapters translate failures without parsing messages. The
 * original cause is preserved when one exists.
 */
export class ActorError extends Error {
  override readonly name = 'ActorError';
  readonly code: ActorErrorCode;
  readonly details?: ActorErrorDetails;

  constructor(code: ActorErrorCode, message: string, details?: ActorErrorDetails, options?: ErrorOptions) {
    super(message, options);
    this.code = code;
    if (details !== undefined) {
      this.details = details;
    }
  }

  toJSON(): { name: string; code: ActorErrorCode; message: string; details?: ActorErrorDetails } {
    return {
      name: this.name,
      code: this.code,
      message: this.message,
      ...(this.details === undefined ? {} : { details: this.details }),
    };
  }
}

export function isActorError(error: unknown): error is ActorError {
  return error instanceof ActorError;
}
