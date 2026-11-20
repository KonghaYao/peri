import { z } from 'zod';
import { PlatformRequestError } from '../errors';

/** Wire limits shared by vaults and credentials. */
const MAX_NAME_BYTES = 256;
const MAX_METADATA_PAIRS = 16;
const MAX_METADATA_KEY_LENGTH = 64;
const MAX_METADATA_VALUE_LENGTH = 512;
const MAX_SECRET_NAME_LENGTH = 256;
const MAX_SECRET_LENGTH = 1_048_576;

/**
 * Metadata is an exact string map: keys are 1..64 characters, values up to 512 characters,
 * and at most 16 pairs, matching the documented update and filter limits.
 */
const metadataSchema = z
  .record(z.string().min(1).max(MAX_METADATA_KEY_LENGTH), z.string().max(MAX_METADATA_VALUE_LENGTH))
  .refine((metadata) => Object.keys(metadata).length <= MAX_METADATA_PAIRS, {
    message: `metadata supports at most ${MAX_METADATA_PAIRS} pairs`,
  });

/** Names are trimmed before storage and must be 1..256 UTF-8 bytes afterwards. */
const nameSchema = z
  .string()
  .transform((value) => value.trim())
  .pipe(
    z
      .string()
      .min(1)
      .refine((value) => Buffer.byteLength(value, 'utf8') <= MAX_NAME_BYTES, {
        message: `name must be at most ${MAX_NAME_BYTES} UTF-8 bytes`,
      }),
  );

/** Destination permissions attached to an environment-variable credential. */
export type CredentialNetworkingDescriptor =
  | { readonly type: 'unrestricted' }
  | { readonly type: 'limited'; readonly allowed_hosts: readonly string[] };

/**
 * The non-secret part of a credential's auth configuration, as returned on the wire. Secrets
 * (bearer tokens, secret values, refresh tokens) are never part of this shape.
 */
export type CredentialAuthDescriptor =
  | { readonly type: 'static_bearer'; readonly mcp_server_url: string }
  | {
      readonly type: 'environment_variable';
      readonly secret_name: string;
      readonly networking: CredentialNetworkingDescriptor;
    };

export interface ParsedCredentialAuth {
  readonly descriptor: CredentialAuthDescriptor;
  /** Write-only secret supplied by the caller; stored encrypted and never emitted. */
  readonly secret: string;
}

/** Raised for auth types outside the two supported credential kinds. */
export class CredentialAuthUnsupportedError extends Error {
  constructor() {
    super('Unsupported credential auth type');
    this.name = 'CredentialAuthUnsupportedError';
  }
}

export const CreateVaultBody = z.strictObject({
  name: nameSchema,
  metadata: metadataSchema.nullable().optional(),
});

/** Omitted fields keep their stored value; `name: null` clears the documented nullable name. */
export const UpdateVaultBody = z.strictObject({
  name: nameSchema.nullable().optional(),
  metadata: metadataSchema.optional(),
});

export const CreateCredentialBody = z.strictObject({
  name: nameSchema,
  auth: z.unknown(),
  metadata: metadataSchema.optional(),
});

/**
 * Patch semantics: omitted fields keep their stored value, supplied fields replace it.
 * `name` is not part of the documented rotation params, so it is rejected (the strict
 * object below) instead of silently accepted.
 */
export const UpdateCredentialBody = z
  .strictObject({
    auth: z.unknown().optional(),
    metadata: metadataSchema.optional(),
  })
  .refine((body) => body.auth !== undefined || body.metadata !== undefined, {
    message: 'At least one of auth or metadata must be supplied',
  });

/** Hostnames and IPv4 addresses only: no scheme, path, port, wildcard or IPv6. */
const HOST_PATTERN = /^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/;
const MAX_ALLOWED_HOSTS = 16;

const networkingSchema = z
  .discriminatedUnion('type', [
    z.strictObject({ type: z.literal('unrestricted') }),
    z.strictObject({
      type: z.literal('limited'),
      allowed_hosts: z
        .array(z.string().min(1).max(253))
        .min(1)
        .max(MAX_ALLOWED_HOSTS)
        .transform((hosts) => hosts.map((host) => host.toLowerCase()))
        .refine((hosts) => hosts.every((host) => HOST_PATTERN.test(host)), {
          message: 'allowed_hosts must be hostnames or IPv4 addresses',
        })
        .refine((hosts) => new Set(hosts).size === hosts.length, {
          message: 'allowed_hosts must be distinct',
        }),
    }),
  ])
  .transform(
    (networking): CredentialNetworkingDescriptor =>
      networking.type === 'limited'
        ? { type: 'limited', allowed_hosts: [...networking.allowed_hosts] }
        : { type: 'unrestricted' },
  );

const staticBearerAuthSchema = z.strictObject({
  type: z.literal('static_bearer'),
  mcp_server_url: z.string().max(MAX_SECRET_LENGTH).optional(),
  token: z.string().max(MAX_SECRET_LENGTH),
});

const environmentVariableAuthSchema = z.strictObject({
  type: z.literal('environment_variable'),
  secret_name: z.string().min(1).max(MAX_SECRET_NAME_LENGTH),
  secret_value: z.string().min(1).max(MAX_SECRET_LENGTH),
  networking: networkingSchema.optional(),
});

function parseAuthSchema<T>(schema: z.ZodType<T>, value: unknown): T {
  const result = schema.safeParse(value);
  if (!result.success) {
    const issue = result.error.issues[0];
    const segments = issue === undefined ? [] : issue.path.map(String);
    // Unrecognized-key issues carry the offending names separately from `path`.
    const unknownKey = (issue as { keys?: readonly string[] } | undefined)?.keys?.[0];
    if (unknownKey !== undefined) segments.push(unknownKey);
    const path = ['auth', ...segments].join('.');
    throw new PlatformRequestError('Request validation failed', path, result.error);
  }
  return result.data;
}

/**
 * Validates the write-only auth payload and splits it into the public descriptor and the
 * secret. Unknown auth types are reported separately so the route can answer with
 * `credential_auth_unsupported` instead of a generic validation failure.
 */
export function parseCredentialAuth(raw: unknown): ParsedCredentialAuth {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new PlatformRequestError('Request validation failed', 'auth');
  }
  const type = (raw as Record<string, unknown>).type;
  if (typeof type !== 'string' || type.length === 0) {
    throw new PlatformRequestError('Request validation failed', 'auth.type');
  }
  switch (type) {
    case 'static_bearer': {
      const auth = parseAuthSchema(staticBearerAuthSchema, raw);
      return {
        descriptor: { type: 'static_bearer', mcp_server_url: auth.mcp_server_url ?? '' },
        secret: auth.token,
      };
    }
    case 'environment_variable': {
      const auth = parseAuthSchema(environmentVariableAuthSchema, raw);
      return {
        descriptor: {
          type: 'environment_variable',
          secret_name: auth.secret_name,
          networking: auth.networking ?? { type: 'unrestricted' },
        },
        secret: auth.secret_value,
      };
    }
    default:
      throw new CredentialAuthUnsupportedError();
  }
}

/** Rotation carries only the write-only secret; the stored descriptor stays authoritative. */
const rotateStaticBearerAuthSchema = z.strictObject({
  type: z.literal('static_bearer'),
  token: z.string().max(MAX_SECRET_LENGTH),
});

const rotateEnvironmentVariableAuthSchema = z.strictObject({
  type: z.literal('environment_variable'),
  secret_value: z.string().min(1).max(MAX_SECRET_LENGTH),
});

/**
 * Validates an auth update and returns the replacement secret. The credential keeps its stored
 * authentication method and destination configuration, so a different type is rejected instead
 * of being silently coerced.
 */
export function parseCredentialSecretRotation(
  raw: unknown,
  current: CredentialAuthDescriptor['type'],
): string {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new PlatformRequestError('Request validation failed', 'auth');
  }
  const type = (raw as Record<string, unknown>).type;
  if (type !== 'static_bearer' && type !== 'environment_variable') {
    throw new CredentialAuthUnsupportedError();
  }
  if (type !== current) {
    throw new PlatformRequestError('Credential auth type cannot change', 'auth.type');
  }
  if (type === 'static_bearer') {
    return parseAuthSchema(rotateStaticBearerAuthSchema, raw).token;
  }
  return parseAuthSchema(rotateEnvironmentVariableAuthSchema, raw).secret_value;
}
