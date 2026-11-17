import { ActorError } from './errors.js';

export type JsonPrimitive = string | number | boolean | null;

/** The JSON data model accepted at every public boundary in this package. */
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

export type JsonObject = { [key: string]: JsonValue };

export const MAX_JSON_DEPTH = 32;

const ARRAY_INDEX = /^(0|[1-9]\d*)$/;

/**
 * Asserts that a value is JSON data.
 *
 * This is deliberately stricter than `JSON.stringify`: class instances, symbol keys, sparse arrays,
 * non-finite numbers and cycles are rejected instead of being silently normalized or dropped.
 */
export function assertJsonValue(value: unknown, path = '$'): asserts value is JsonValue {
  visitJson(value, path, 0, new Set<object>());
}

export function assertJsonObject(value: unknown, path = '$'): asserts value is JsonObject {
  assertJsonValue(value, path);
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ActorError('invalid_json', `${path} must be a JSON object`, { path });
  }
}

/**
 * Serializes JSON data with deterministic object key order.
 *
 * Callers must validate first with {@link assertJsonValue}; this function is intentionally total
 * for valid JSON and does not re-run validation on every recursion.
 */
export function canonicalJson(value: JsonValue): string {
  if (value === null || typeof value !== 'object') {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(',')}]`;
  }
  const keys = Object.keys(value).sort();
  const members = keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key] as JsonValue)}`);
  return `{${members.join(',')}}`;
}

export function jsonByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function visitJson(value: unknown, path: string, depth: number, ancestors: Set<object>): void {
  if (value === null) {
    return;
  }

  switch (typeof value) {
    case 'string':
    case 'boolean':
      return;
    case 'number':
      if (!Number.isFinite(value)) {
        throw new ActorError('invalid_json', `${path} must be a finite number`, { path });
      }
      return;
    case 'object':
      break;
    default:
      throw new ActorError('invalid_json', `${path} is not JSON data`, { path, type: typeof value });
  }

  if (depth >= MAX_JSON_DEPTH) {
    throw new ActorError('invalid_json', `${path} exceeds the maximum JSON depth`, { path });
  }
  if (ancestors.has(value)) {
    throw new ActorError('invalid_json', `${path} contains a cycle`, { path });
  }

  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      visitArray(value as unknown[], path, depth, ancestors);
      return;
    }
    visitObject(value, path, depth, ancestors);
  } finally {
    ancestors.delete(value);
  }
}

function visitArray(value: unknown[], path: string, depth: number, ancestors: Set<object>): void {
  for (let index = 0; index < value.length; index += 1) {
    if (!(index in value)) {
      throw new ActorError('invalid_json', `${path}[${index}] is a sparse array slot`, { path });
    }
    visitJson(value[index], `${path}[${index}]`, depth + 1, ancestors);
  }

  for (const key of Object.keys(value)) {
    const index = Number(key);
    if (!ARRAY_INDEX.test(key) || index >= value.length) {
      throw new ActorError('invalid_json', `${path} has a non-index array property`, { path });
    }
  }
}

function visitObject(value: object, path: string, depth: number, ancestors: Set<object>): void {
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new ActorError('invalid_json', `${path} must be a plain object`, { path });
  }
  if (Object.getOwnPropertySymbols(value).length > 0) {
    throw new ActorError('invalid_json', `${path} must not contain symbol keys`, { path });
  }

  for (const key of Object.keys(value)) {
    visitJson((value as Record<string, unknown>)[key], `${path}.${key}`, depth + 1, ancestors);
  }
}
