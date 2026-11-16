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
import { chatTargetFingerprint, closeChatActivation, openChatActivation } from './chat-session.js';
import { ChatActivationActivity, type ChatActivityDiagnosticType } from './chat-session-activity.js';
import { toUserError } from './errors.js';
import { type RivetKeepAwakeContext, rivetKeepAwakePort } from './keep-awake.js';
import { notifyLifecycle, type PeriActorLifecycleEvent } from './lifecycle.js';

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
 * Keep-alive projections keyed by SDK handle.
 *
 * Per handle, not per actor: a reopened handle must not inherit the previous handle's counters,
 * and a closed handle must stay unreachable from the vars.
 */
const activities = new WeakMap<ChatServiceContract, ChatActivationActivity>();

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
          return await openChat(c, target, options);
        } catch (error) {
          toUserError(error);
        }
      },
      execute: async (c, raw: unknown, target?: ChatExecuteTarget | null) => {
        try {
          const chat = requireChat(c.vars.chat);
          if (target !== undefined && target !== null) {
            assertTarget(chat, target);
          }
          const action = raw as ChatUserAction;
          const activity = activities.get(chat);
          return await (activity === undefined
            ? executeAction(chat, action)
            : activity.run(() => executeAction(chat, action)));
        } catch (error) {
          toUserError(error);
        }
      },
      readPayload: async (c, ref: ToolPayloadReadRef, payloadOptions?: ReadPayloadOptions) => {
        try {
          const chat = requireChat(c.vars.chat);
          const activity = activities.get(chat);
          return await (activity === undefined
            ? chat.readPayload(ref, payloadOptions)
            : activity.run(() => chat.readPayload(ref, payloadOptions)));
        } catch (error) {
          toUserError(error);
        }
      },
      observe: (c) => {
        try {
          const chat = requireChat(c.vars.chat);
          return activities.get(chat)?.observe() ?? chat.observe?.() ?? null;
        } catch (error) {
          toUserError(error);
        }
      },
      activity: (c) => {
        try {
          // A missing projection means `start` failed; report an incomplete snapshot so the caller
          // never mistakes it for a releasable unit.
          return activities.get(requireChat(c.vars.chat))?.activity() ?? idleActivity();
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
          await closeActivation(c, options);
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

  const fingerprint = chatTargetFingerprint(target);
  if (c.vars.chat !== null && c.vars.open !== null) {
    if (c.vars.open.fingerprint === fingerprint) {
      return {
        chatId: target.chatId,
        workspace: target.workspace,
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

  // A failed earlier close can leave a half-released activation; never stack a second one on it.
  await closeActivation(c, options);

  const activation = await openChatActivation(options, target);
  // A projection that fails to subscribe leaves an otherwise healthy handle; keep the activation
  // usable rather than forcing the caller to retry a construction that already succeeded.
  const activity = new ChatActivationActivity({
    chat: activation.chat,
    keepAwake: rivetKeepAwakePort(c, {
      onError: (error) => diagnose(options, { type: 'keep_awake_failed', error }),
    }),
    onFact: (envelope) => publishFact(c, options, envelope),
    onDiagnostic: (type, error) => diagnose(options, activityDiagnostic(type, error)),
  });
  activities.set(activation.chat, activity);
  try {
    activity.start();
  } catch (error) {
    activities.delete(activation.chat);
    diagnose(options, { type: 'activity_source_failed', error });
  }

  c.vars.chat = activation.chat;
  c.vars.database = activation.database;
  c.vars.open = { target, fingerprint };
  emitLifecycle(options, { type: 'wake', actorId: c.actorId, key: c.key });
  return {
    chatId: target.chatId,
    workspace: target.workspace,
    binding: activation.chat.binding,
    reused: false,
  };
}

/**
 * Releases the activation's chat and database.
 *
 * Vars are cleared before awaiting so a rejecting close cannot leave the actor looking open; the
 * handle is still reachable through the local reference, so the database close always runs.
 */
async function closeActivation(c: ChatContext, options: ChatActorOptions, rethrow = true): Promise<void> {
  const activation = takeActivation(c);
  if (activation === null) {
    return;
  }
  await closeChatActivation(options, activation, rethrow);
}

function takeActivation(c: ChatContext): Pick<ChatActivation, 'chat' | 'database'> | null {
  const { chat, database } = c.vars;
  if (chat !== null) {
    activities.get(chat)?.stop();
    activities.delete(chat);
  }
  c.vars.chat = null;
  c.vars.database = null;
  c.vars.open = null;
  return chat === null || database === null ? null : { chat, database };
}

async function teardown(c: ChatContext, type: 'sleep' | 'destroy', options: ChatActorOptions): Promise<void> {
  try {
    // Rivet is already tearing the activation down; a release failure only needs a diagnostic.
    await closeActivation(c, options, false);
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
