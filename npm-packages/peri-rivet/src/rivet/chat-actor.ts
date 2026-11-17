import type {
  ChatDatabase,
  ChatFactEnvelope,
  ChatService as ChatServiceContract,
  ChatUserAction,
  ReadPayloadOptions,
  SendReceipt,
  ToolPayloadReadRef,
  ToolPayloadResponse,
} from '@peri-code/sdk';
import type { ChatObservation } from '@peri-code/sdk/observe';
import { type ActionContext, type ActorDefinition, actor, event, type Type } from 'rivetkit';
import { PeriError } from '../contracts/errors.js';
import { type ChatId, parseChatId } from '../contracts/identity.js';
import { ActorError } from '../errors.js';
import type { RuntimeActivity } from '../host/activity.js';
import { readFacts } from './chat-facts.js';
import type {
  ChatActivation,
  ChatActorDiagnostic,
  ChatActorOptions,
  ChatExecuteResult,
  ChatExecuteTarget,
  ChatFactsResult,
  ChatOpenResult,
  ChatOpenTarget,
} from './chat-ports.js';
import { chatTargetFingerprint, openChatActivation, resolveChatOpenTarget } from './chat-session.js';
import { ChatActivationActivity, type ChatActivityDiagnosticType } from './chat-session-activity.js';
import { toUserError } from './errors.js';
import type { HostResourceLease } from './host-resource-owner.js';
import { type RivetKeepAwakeContext, rivetKeepAwakePort } from './keep-awake.js';
import { notifyLifecycle, type PeriActorLifecycleEvent } from './lifecycle.js';

/** One activation's host-owned value plus its transient actor-bound observation projection. */
type ChatLive = {
  readonly activation: ChatActivation;
  activity: ChatActivationActivity | null;
};

/** One activation's resources; all three clear together so no half-open state is observable. */
export type ChatActorVars = {
  chat: ChatServiceContract | null;
  database: (ChatDatabase & { close(): Promise<void> }) | null;
  open: { readonly target: ChatOpenTarget; readonly fingerprint: string } | null;
};

type ChatActorEvents = { chatFact: Type<ChatFactEnvelope> };

type ChatActionContext = ActionContext<
  unknown,
  unknown,
  unknown,
  ChatActorVars,
  unknown,
  undefined,
  ChatActorEvents
>;

type ChatContext = {
  readonly actorId: string;
  readonly key: readonly string[];
  readonly vars: ChatActorVars;
  readonly keepAwake: RivetKeepAwakeContext['keepAwake'];
  readonly broadcast: (name: 'chatFact', data: ChatFactEnvelope) => void;
};

export type ChatActorActions = {
  open(c: ChatActionContext, target: ChatOpenTarget): Promise<ChatOpenResult>;
  execute(c: ChatActionContext, raw: unknown, target?: ChatExecuteTarget | null): Promise<ChatExecuteResult>;
  readPayload(
    c: ChatActionContext,
    ref: ToolPayloadReadRef,
    options?: ReadPayloadOptions,
  ): Promise<ToolPayloadResponse>;
  observe(c: ChatActionContext): ChatObservation | null;
  activity(c: ChatActionContext): RuntimeActivity;
  facts(c: ChatActionContext): ChatFactsResult;
  close(c: ChatActionContext): Promise<{ readonly closed: true }>;
};

/** Explicit so consumers and declaration emit never depend on Rivet's internal generic names. */
export type PeriChatActorDefinition = ActorDefinition<
  unknown,
  unknown,
  unknown,
  ChatActorVars,
  unknown,
  undefined,
  ChatActorEvents,
  Record<never, never>,
  ChatActorActions
>;

/**
 * Per-activation lifecycle serialization.
 *
 * Actions on one actor really do run concurrently. Open/close/teardown share one queue per vars so
 * a second open cannot construct a second database over the first, and close/teardown cannot race
 * an in-flight open. Resource ownership and close progress live in the injected host owner, which
 * survives the framework clearing these vars.
 */
const lifecycleQueues = new WeakMap<ChatActorVars, Promise<void>>();

function serializeLifecycle<T>(vars: ChatActorVars, run: () => Promise<T> | T): Promise<T> {
  const previous = lifecycleQueues.get(vars) ?? Promise.resolve();
  const task = previous.then(run);
  lifecycleQueues.set(
    vars,
    task.then(
      () => undefined,
      () => undefined,
    ),
  );
  return task;
}

/**
 * A Rivet actor hosting exactly one Peri chat, keyed by `[chatId]`.
 *
 * The actor is the only authority over its activation: `open` builds the SDK handle from a full
 * target and every other action fails with `closed` until it runs. `chatId` is the client-generated
 * chat identity; the ACP Session lives inside the handle and may be switched via `change_session`.
 * Nothing is persisted — no actor state, no actor database, no fact cursor. Sleep closes the chat
 * and its database, so the next wake starts from history instead of resuming an execution.
 */
export function createPeriChatActor(options: ChatActorOptions): PeriChatActorDefinition {
  return actor({
    ...(options.options === undefined ? {} : { options: options.options }),
    events: {
      chatFact: event<ChatFactEnvelope>(),
    },
    createVars: (): ChatActorVars => ({ chat: null, database: null, open: null }),
    onSleep: (c) => teardown(c, 'sleep', options),
    onDestroy: (c) => teardown(c, 'destroy', options),
    actions: {
      open: async (c, target: ChatOpenTarget) => {
        try {
          return await serializeLifecycle(c.vars, () => openChat(c, target, options));
        } catch (error) {
          toUserError(error);
        }
      },
      execute: async (c, raw: unknown, target?: ChatExecuteTarget | null) => {
        try {
          const { chat, activity } = requireLiveChat(c, options);
          if (target !== undefined && target !== null) {
            assertTarget(chat, target);
          }
          const action = raw as ChatUserAction;
          return await (activity === null
            ? executeAction(chat, action)
            : activity.run(() => executeAction(chat, action)));
        } catch (error) {
          toUserError(error);
        }
      },
      readPayload: async (c, ref: ToolPayloadReadRef, payloadOptions?: ReadPayloadOptions) => {
        try {
          const { chat, activity } = requireLiveChat(c, options);
          return await (activity === null
            ? chat.readPayload(ref, payloadOptions)
            : activity.run(() => chat.readPayload(ref, payloadOptions)));
        } catch (error) {
          toUserError(error);
        }
      },
      observe: (c) => {
        try {
          const chat = requireChat(c.vars.chat);
          return options.owner.lease<ChatLive>(c.vars)?.value.activity?.observe() ?? chat.observe?.() ?? null;
        } catch (error) {
          toUserError(error);
        }
      },
      activity: (c) => {
        try {
          requireChat(c.vars.chat);
          // A missing projection means `start` failed; report an incomplete snapshot so the caller
          // never mistakes it for a releasable unit.
          return options.owner.lease<ChatLive>(c.vars)?.value.activity?.activity() ?? idleActivity();
        } catch (error) {
          toUserError(error);
        }
      },
      facts: (c) => {
        try {
          return readFacts(requireChat(c.vars.chat));
        } catch (error) {
          toUserError(error);
        }
      },
      close: async (c) => {
        try {
          await serializeLifecycle(c.vars, () => closeActivation(c, options));
          return { closed: true };
        } catch (error) {
          toUserError(error);
        }
      },
    },
  });
}

async function openChat(
  c: ChatContext,
  target: ChatOpenTarget,
  options: ChatActorOptions,
): Promise<ChatOpenResult> {
  requireTargetChat(c.key, target);

  // Parse the workspace boundary before touching ownership, the database or the runtime: a foreign
  // or non-canonical URI must leave the current activation exactly as it was.
  const resolved = resolveChatOpenTarget(target, options.machineId);
  const fingerprint = chatTargetFingerprint(resolved.target);
  const existing = options.owner.lease<ChatLive>(c.vars);
  if (existing !== undefined && !existing.retired && c.vars.chat !== null && c.vars.open !== null) {
    if (existing.value.activation.chat !== c.vars.chat) {
      throw internalStateError();
    }
    if (c.vars.open.fingerprint === fingerprint) {
      return {
        chatId: target.chatId,
        workspace: resolved.workspaceUri,
        binding: c.vars.chat.binding,
        reused: true,
      };
    }
    throw new ActorError('binding_conflict', 'the actor is already open for a different target', {
      reason: 'binding_conflict',
      openChatId: c.vars.open.target.chatId,
      requestedChatId: target.chatId,
    });
  }

  // A failed earlier close leaves a retired owner record. Finish it before a new database or SDK
  // handle is created; an unconfirmed close blocks this open with its original failure.
  if (existing !== undefined) {
    await closeLease(c, existing);
  } else if (c.vars.chat !== null || c.vars.database !== null) {
    throw internalStateError();
  }

  const lease = await options.owner.open<ChatLive>(c.key, c.vars, async (track) => {
    const activation = await openChatActivation(options, resolved, track);
    return { activation, activity: null };
  });
  const live = lease.value;
  try {
    const activity = new ChatActivationActivity({
      chat: live.activation.chat,
      keepAwake: rivetKeepAwakePort(c, {
        onError: (error) => diagnose(options, { type: 'keep_awake_failed', error }),
      }),
      onFact: (envelope) => publishFact(c, options, envelope),
      onDiagnostic: (type, error) => diagnose(options, activityDiagnostic(type, error)),
    });
    live.activity = activity;
    // The owner keeps the real SDK/database after retire; the observer/keep-awake projections must
    // not survive the activation that supplied `c.broadcast` and `c.keepAwake`.
    lease.onRetire(() => {
      live.activity?.stop();
      live.activity = null;
    });
    try {
      activity.start();
    } catch (error) {
      activity.stop();
      live.activity = null;
      diagnose(options, { type: 'activity_source_failed', error });
    }
  } catch (error) {
    try {
      await lease.close();
    } catch (closeError) {
      diagnose(options, { type: 'activity_source_failed', error: closeError });
    }
    throw error;
  }

  c.vars.chat = live.activation.chat;
  c.vars.database = live.activation.database;
  c.vars.open = { target, fingerprint };
  emitLifecycle(options, { type: 'wake', actorId: c.actorId, key: c.key });
  return {
    chatId: target.chatId,
    workspace: resolved.workspaceUri,
    binding: live.activation.chat.binding,
    reused: false,
  };
}

/**
 * Releases the activation's chat and database through the host owner.
 *
 * The owner closes chat before database, keeps confirmed progress, and single-flights the close.
 * Nothing is cleared until every resource confirmed; a failed close leaves the record owned by the
 * host, which outlives Rivet's activation vars.
 */
async function closeLease(c: ChatContext, lease: HostResourceLease<ChatLive>): Promise<void> {
  await lease.close();
  clearActivation(c);
}

async function closeActivation(c: ChatContext, options: ChatActorOptions): Promise<void> {
  const lease = options.owner.lease<ChatLive>(c.vars);
  if (lease === undefined) {
    if (c.vars.chat !== null || c.vars.database !== null) {
      throw internalStateError();
    }
    clearActivation(c);
    return;
  }
  await closeLease(c, lease);
}

function clearActivation(c: ChatContext): void {
  c.vars.chat = null;
  c.vars.database = null;
  c.vars.open = null;
}

async function teardown(c: ChatContext, type: 'sleep' | 'destroy', options: ChatActorOptions): Promise<void> {
  try {
    // Retire first so facts/keep-awake stop before the close starts. If the close cannot confirm,
    // the host owner retains the real SDK/database record for closeAll or the next open.
    options.owner.retire(c.vars);
    await serializeLifecycle(c.vars, () => closeActivation(c, options));
  } catch (error) {
    // Hooks have no client response boundary, but known SDK/database failures must stay explicit
    // instead of being swallowed; the host owner keeps the activation and its progress.
    toUserError(error);
  } finally {
    emitLifecycle(options, { type, actorId: c.actorId, key: c.key });
  }
}

async function executeAction(chat: ChatServiceContract, action: ChatUserAction): Promise<ChatExecuteResult> {
  // `send` is the only action whose SDK result is a synchronous receipt rather than a result value.
  if (action.type === 'send') {
    const receipt: SendReceipt = chat.execute(action);
    await receipt.accepted;
    return {
      type: 'send',
      requestId: receipt.requestId,
      inputId: receipt.inputId,
      target: receipt.target,
    };
  }
  const result = await chat.execute(action as Parameters<ChatServiceContract['execute']>[0] & ChatUserAction);
  return { type: action.type, result } as ChatExecuteResult;
}

/** The actor key is the single statement of which chat this actor owns. */
function requireChatId(key: readonly string[]): ChatId {
  const raw = key[0];
  if (key.length !== 1 || raw === undefined) {
    throw keyShapeError(key.length);
  }
  try {
    return parseChatId(raw);
  } catch {
    // The actor boundary owns its own reason vocabulary; the identity rule stays in `contracts`.
    throw keyShapeError(key.length);
  }
}

function keyShapeError(keyLength: number): ActorError {
  return new ActorError('invalid_identity', 'the chat actor key must be exactly [chatId]', {
    reason: 'actor_key_shape',
    keyLength,
  });
}

function requireTargetChat(key: readonly string[], target: ChatOpenTarget): void {
  const chatId = requireChatId(key);
  if (target === null || typeof target !== 'object') {
    throw new ActorError('invalid_input', 'open requires a target', { reason: 'missing_target' });
  }
  if (target.chatId !== chatId) {
    throw new ActorError('invalid_identity', 'open target chatId does not match the actor key', {
      reason: 'actor_key_mismatch',
      key: chatId,
      target: target.chatId,
    });
  }
}

function requireChat(chat: ChatServiceContract | null): ChatServiceContract {
  if (chat === null) {
    throw closedError('the chat actor has not been opened');
  }
  return chat;
}

/**
 * Admission gate for actions that can mutate the SDK handle.
 *
 * Shutdown closes new execute work immediately; a retired lease is already closing and must not
 * accept another action even though its SDK/database record is still host-owned.
 */
function requireLiveChat(
  c: ChatContext,
  options: ChatActorOptions,
): { chat: ChatServiceContract; activity: ChatActivationActivity | null } {
  if (options.owner.shuttingDown) {
    throw closedError('the host is shutting down');
  }
  const lease = options.owner.lease<ChatLive>(c.vars);
  if (lease === undefined || lease.retired) {
    throw closedError('the chat actor is closing');
  }
  const chat = requireChat(c.vars.chat);
  if (lease.value.activation.chat !== chat) {
    throw internalStateError();
  }
  return { chat, activity: lease.value.activity };
}

function internalStateError(): PeriError {
  return new PeriError({
    code: 'internal_error',
    phase: 'internal',
    message: 'the chat activation is not owned by the host resource record',
  });
}

/** Idle snapshot used when the projection is absent; `complete: false` keeps the unit pinned. */
function idleActivity(): RuntimeActivity {
  return {
    revision: 0,
    complete: false,
    rootRunning: false,
    activeInternalTasks: 0,
    queuedInputs: 0,
    pendingDeliveries: 0,
    pendingInteractions: 0,
    localOperations: 0,
    closing: false,
  };
}

function closedError(message: string): PeriError {
  return new PeriError({ code: 'closed', phase: 'session', message, details: { reason: 'not_open' } });
}

function assertTarget(chat: ChatServiceContract, target: ChatExecuteTarget): void {
  const current = chat.binding;
  if (
    current === null ||
    current.sessionId !== target.sessionId ||
    current.bindingRevision !== target.bindingRevision
  ) {
    throw new ActorError('binding_conflict', 'action target does not match the current chat binding', {
      reason: 'binding_conflict',
      current:
        current === null ? null : { sessionId: current.sessionId, bindingRevision: current.bindingRevision },
    });
  }
}

function publishFact(c: ChatContext, options: ChatActorOptions, envelope: ChatFactEnvelope): void {
  try {
    c.broadcast('chatFact', envelope);
  } catch (error) {
    diagnose(options, { type: 'fact_observer_failed', error });
  }
}

function activityDiagnostic(type: ChatActivityDiagnosticType, error: unknown): ChatActorDiagnostic {
  return { type, error };
}

function emitLifecycle(options: ChatActorOptions, event: PeriActorLifecycleEvent): void {
  notifyLifecycle(options.onLifecycle, event, (diagnostic) => diagnose(options, diagnostic));
}

function diagnose(options: ChatActorOptions, diagnostic: ChatActorDiagnostic): void {
  try {
    options.onDiagnostic?.(diagnostic);
  } catch {
    // Diagnostics are observational; they must not affect lifecycle or action outcomes.
  }
}
