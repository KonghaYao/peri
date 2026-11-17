import { createChatSession, type WorkspaceRef } from '@peri-code/sdk';
import { ActorError } from '../errors.js';
import type { ChatActivation, ChatActorDiagnostic, ChatActorOptions, ChatOpenTarget } from './chat-ports.js';
import type { HostResourceTrack } from './host-resource-owner.js';
import { resetOwnership } from './ownership-reset.js';
import { parseMachineId, parseWorkspaceUri } from './workspace-uri.js';

/** Target with the URI already parsed into the SDK's internal `WorkspaceRef`. */
export interface ResolvedChatTarget {
  readonly target: Omit<ChatOpenTarget, 'workspace'> & { readonly workspace: WorkspaceRef };
  readonly workspaceUri: string;
}

/**
 * Parses `open.workspace` and checks it against the deployment's machine identity.
 *
 * This runs before ownership reset, `openDatabase` and runtime construction, so a foreign or
 * non-canonical URI has no side effects. The URI is a locator, not host authentication: only
 * `options.machineId` may name the machine this deployment runs on.
 */
export function resolveChatOpenTarget(target: ChatOpenTarget, machineId: string): ResolvedChatTarget {
  const deploymentMachine = parseMachineId(machineId);
  if (typeof target.workspace !== 'string') {
    throw new ActorError('invalid_identity', 'open.workspace must be a canonical workspace URI', {
      reason: 'invalid_workspace',
      field: 'workspace',
    });
  }
  const parsed = parseWorkspaceUri(target.workspace);
  if (parsed.machineId !== deploymentMachine) {
    throw new ActorError('invalid_identity', 'workspace URI belongs to a different machine', {
      reason: 'machine_mismatch',
      field: 'workspace',
    });
  }
  return {
    target: {
      ...target,
      workspace: { id: parsed.uri, runtimePath: parsed.runtimePath },
    },
    workspaceUri: parsed.uri,
  };
}

/**
 * Opens one activation: clear the previous owner, then build the database and the chat handle.
 *
 * Order matters. The reset runs first because the SDK writes a new owner during construction, and
 * a database left open by a failed construction would hold a lock the next `open` cannot take.
 *
 * The database is registered with the host record as soon as it exists, before
 * `createChatSession` can fail or a shutdown can begin. Chat is registered with order 0 and the
 * database with order 1, so a close always confirms the SDK handle before touching the database it
 * may still be using.
 */
export async function openChatActivation(
  options: ChatActorOptions,
  resolved: ResolvedChatTarget,
  track: HostResourceTrack,
): Promise<ChatActivation> {
  const target = resolved.target;
  try {
    await resetOwnership(options.ownership, target.chatId);
  } catch (error) {
    // A record that cannot be read or cleared must not be silently abandoned: fail this open so
    // the caller retries, instead of letting the SDK surface a misleading ownership conflict.
    diagnose(options, { type: 'ownership_reset_failed', error });
    throw error;
  }

  const database = options.openDatabase(target.storage);
  track(
    {
      close: async () => {
        try {
          await database.close();
        } catch (error) {
          diagnose(options, { type: 'close_failed', scope: 'database', error });
          throw error;
        }
      },
    },
    1,
  );

  const chat = await createChatSession({
    target: {
      // The SDK keys its handle and ownership record by this field; it is the chat identity, not
      // the ACP session, which the SDK creates on the first `session/new`.
      sessionId: target.chatId,
      workspace: target.workspace,
      agent: target.agent,
      storage: target.storage,
      ...(target.sessionPath === undefined ? {} : { sessionPath: target.sessionPath }),
      ...(target.session === undefined ? {} : { session: target.session }),
      ...(target.lifecycle === undefined ? {} : { lifecycle: target.lifecycle }),
    },
    database,
    ownership: options.ownership,
    machineId: options.machineId,
    chatConfig: options.chatConfig,
  });

  track(
    {
      close: async () => {
        try {
          await chat.close();
        } catch (error) {
          diagnose(options, { type: 'close_failed', scope: 'chat', error });
          throw error;
        }
      },
    },
    0,
  );

  return { chat, database };
}

/**
 * Identity that decides whether a repeated `open` may reuse the activation it already has.
 *
 * Every activation input participates: workspace, agent, storage, session, sessionPath and
 * lifecycle. Re-running `open` with any different value must be reported as a conflict instead of
 * silently keeping the old resources. Non-plain values (for example an ACP MCP instance or a
 * factory function) are compared by identity, so two different values never compare equal.
 */
export function chatTargetFingerprint(target: ResolvedChatTarget['target']): string {
  return fingerprintValue({
    chatId: target.chatId,
    workspace: target.workspace,
    agent: target.agent,
    storage: target.storage,
    ...(target.sessionPath === undefined ? {} : { sessionPath: target.sessionPath }),
    ...(target.session === undefined ? {} : { session: target.session }),
    ...(target.lifecycle === undefined ? {} : { lifecycle: target.lifecycle }),
  });
}

const referenceIds = new WeakMap<object, number>();
const symbolReferences = new Map<symbol, number>();
let nextReferenceId = 1;

function referenceId(value: object): number {
  let id = referenceIds.get(value);
  if (id === undefined) {
    id = nextReferenceId;
    nextReferenceId += 1;
    referenceIds.set(value, id);
  }
  return id;
}

function symbolReferenceId(value: symbol): number {
  let id = symbolReferences.get(value);
  if (id === undefined) {
    id = nextReferenceId;
    nextReferenceId += 1;
    symbolReferences.set(value, id);
  }
  return id;
}

/**
 * Deterministic text for activation inputs without requiring them to be strict JSON.
 *
 * Plain objects compare by sorted keys; class instances, functions and cycles compare by object
 * identity. Recording a reference instead of a value keeps "unknown" distinct rather than
 * conflating two different configurations.
 */
function fingerprintValue(value: unknown, ancestors = new Set<object>()): string {
  if (value === null) return 'null';
  switch (typeof value) {
    case 'string':
      return JSON.stringify(value);
    case 'number':
      return Number.isFinite(value) ? `number:${value}` : `number:${String(value)}`;
    case 'boolean':
      return value ? 'true' : 'false';
    case 'undefined':
      return 'undefined';
    case 'bigint':
      return `bigint:${value.toString()}`;
    case 'function':
      return `reference:${referenceId(value)}`;
    case 'symbol':
      return `symbol:${symbolReferenceId(value)}`;
    case 'object':
      break;
    default:
      return `${typeof value}`;
  }
  if (ancestors.has(value)) return `cycle:${referenceId(value)}`;
  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      return `[${value.map((item) => fingerprintValue(item, ancestors)).join(',')}]`;
    }
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) {
      return `reference:${referenceId(value)}`;
    }
    const record = value as Record<string, unknown>;
    const entries = Object.keys(record)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${fingerprintValue(record[key], ancestors)}`);
    return `{${entries.join(',')}}`;
  } finally {
    ancestors.delete(value);
  }
}

function diagnose(options: ChatActorOptions, diagnostic: ChatActorDiagnostic): void {
  try {
    options.onDiagnostic?.(diagnostic);
  } catch {
    // Diagnostics are observational; they must not change the outcome of open or close.
  }
}
