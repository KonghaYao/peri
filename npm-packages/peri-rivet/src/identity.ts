import { ActorError } from './errors.js';

declare const identityBrand: unique symbol;

type Brand<T, B extends string> = T & { readonly [identityBrand]: B };

export type RuntimeId = Brand<string, 'RuntimeId'>;
export type RootSessionId = Brand<string, 'RootSessionId'>;
export type ResourceId = Brand<string, 'ResourceId'>;
export type ResourceScope = Brand<string, 'ResourceScope'>;
export type InputId = Brand<string, 'InputId'>;

const IDENTITY_WHITESPACE = /\s/u;
const DEFAULT_MAX_LENGTH = 200;

function hasForbiddenIdentityCharacter(value: string): boolean {
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (codePoint === undefined) {
      continue;
    }
    if (codePoint <= 0x1f || codePoint === 0x7f || IDENTITY_WHITESPACE.test(character)) {
      return true;
    }
  }
  return false;
}

/**
 * Validation lives here rather than at each call site so an empty or control-character identity can
 * never reach a store, log correlation field, or actor key.
 */
function parseIdentity<B extends string>(
  label: string,
  value: string,
  maxLength = DEFAULT_MAX_LENGTH,
): Brand<string, B> {
  if (typeof value !== 'string' || value.length === 0 || value.length > maxLength) {
    throw new ActorError(
      'invalid_identity',
      `${label} must be a non-empty string of at most ${maxLength} characters`,
      {
        label,
        length: typeof value === 'string' ? value.length : 0,
      },
    );
  }
  if (hasForbiddenIdentityCharacter(value)) {
    throw new ActorError('invalid_identity', `${label} must not contain whitespace or control characters`, {
      label,
    });
  }
  return value as Brand<string, B>;
}

export function parseRuntimeId(value: string): RuntimeId {
  return parseIdentity('runtimeId', value);
}

export function parseRootSessionId(value: string): RootSessionId {
  return parseIdentity('rootSessionId', value);
}

export function parseResourceId(value: string): ResourceId {
  return parseIdentity('resourceId', value);
}

export function parseResourceScope(value: string): ResourceScope {
  return parseIdentity('scope', value, 256);
}

export function parseInputId(value: string): InputId {
  return parseIdentity('inputId', value);
}
