/** Stable error codes for the local swarm sandbox service. */
export type SwarmErrorCode = 'invalid_config' | 'invalid_logical_env_id' | 'bindings_invalid' | 'bindings_insecure';

export class SwarmError extends Error {
  readonly code: SwarmErrorCode;

  constructor(code: SwarmErrorCode, message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = 'SwarmError';
    this.code = code;
  }
}

/** Logical environment ids are URL segments and bindings keys, never paths. */
export const logicalEnvIdPattern = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

export function assertLogicalEnvId(value: string): string {
  if (!logicalEnvIdPattern.test(value))
    throw new SwarmError('invalid_logical_env_id', 'logicalEnvId must match [A-Za-z0-9][A-Za-z0-9._-]{0,127}.');
  return value;
}
