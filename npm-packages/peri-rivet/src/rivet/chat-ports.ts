import type {
  CancelRequestResult,
  ChatAgentConfig,
  ChatBindingTarget,
  ChatConfig,
  ChatDatabase,
  ChatFactEnvelope,
  ChatService as ChatServiceContract,
  ControlResult,
  HistoryPageOutcome,
  KvStorage,
  SessionReadyResult,
} from '@peri-code/sdk';
import type { ActorOptionsInput } from 'rivetkit';
import type { HostResourceOwner } from './host-resource-owner.js';
import type { PeriActorLifecycleEvent } from './lifecycle.js';

/** Shared Store endpoint used to snapshot agent config; the SDK never parses it here. */
export interface ChatStorageConfig {
  readonly url: string;
  readonly authToken?: string;
}

/**
 * The whole identity of one chat activation.
 *
 * `chatId` must equal the actor key, so a target can never point the actor at another chat. It is
 * the client-generated chat identity and stays fixed for the actor's lifetime; the ACP Session it
 * runs is chosen later and may change. Everything else is fixed for the activation's lifetime, or
 * overrides `ChatActorOptions.chatConfig` for this open only.
 */
export interface ChatOpenTarget {
  readonly chatId: string;
  /**
   * Canonical `peri+sandbox://` URI, exactly `WorkspaceURI.toString()`.
   *
   * Only the deployment's `machineId` is trusted; the URI is parsed and checked against it before
   * any activation resource is touched.
   */
  readonly workspace: string;
  readonly agent: ChatAgentConfig;
  readonly storage: ChatStorageConfig;
  readonly sessionPath?: string;
  readonly session?: ChatConfig['session'];
  readonly lifecycle?: ChatConfig['lifecycle'];
}

/** Session ownership storage: the only KV the actor writes, and only to clear a stale record. */
export type ChatOwnership = Pick<KvStorage, 'getItem' | 'setItem' | 'removeItem'>;

/**
 * Opens the actor's private session directory.
 *
 * The handle must be owned exclusively by this activation: no other instance may read the same
 * database while the actor holds it open.
 */
export type OpenChatDatabase = (storage: ChatStorageConfig) => ChatDatabase & { close(): Promise<void> };

/**
 * Binding identity a caller composed an action against; checked before the action runs.
 *
 * `sessionId` is the ACP Session the SDK currently holds, not the chat actor key.
 */
export interface ChatExecuteTarget {
  readonly sessionId: string;
  readonly bindingRevision: number;
}

export type ChatExecuteResult =
  | Readonly<{
      type: 'send';
      requestId: string;
      inputId: string;
      target: ChatBindingTarget;
    }>
  | Readonly<{ type: 'create_new_session' | 'change_session'; result: SessionReadyResult }>
  | Readonly<{ type: 'load_earlier_history'; result: HistoryPageOutcome }>
  | Readonly<{ type: 'cancel'; result: CancelRequestResult }>
  | Readonly<{ type: 'force_send' | 'take_back' | 'answer' | 'set_config_option'; result: ControlResult }>;

/**
 * Facts are not cursored: a client replays the current generation and keeps its own position.
 *
 * `bindingRevision` identifies the generation, so a client can discard a replay that belongs to a
 * binding it has already left.
 */
export interface ChatFactsResult {
  readonly binding: ChatBindingTarget | null;
  readonly envelopes: ChatFactEnvelope[];
  /** The fact log overflowed, so the replay is incomplete and the client must rebind. */
  readonly overflow: boolean;
}

/** Result of opening (or reusing) the chat handle for this actor key. */
export interface ChatOpenResult {
  readonly chatId: string;
  /** The canonical `peri+sandbox://` URI this activation was opened with. */
  readonly workspace: string;
  /** `null` until the SDK binds a Session; binding is the SDK's decision, not the actor's. */
  readonly binding: ChatBindingTarget | null;
  /** True when an earlier `open` with the same identity was reused instead of rebuilt. */
  readonly reused: boolean;
}

export type ChatActorDiagnostic =
  | Readonly<{ type: 'ownership_reset_failed'; error: unknown }>
  | Readonly<{ type: 'close_failed'; scope: 'chat' | 'database'; error: unknown }>
  | Readonly<{ type: 'lifecycle_observer_failed'; error: unknown }>
  | Readonly<{ type: 'fact_observer_failed'; error: unknown }>
  | Readonly<{ type: 'activity_source_failed'; error: unknown }>
  | Readonly<{ type: 'keep_awake_failed'; error: unknown }>;

export interface ChatActorOptions {
  /**
   * Required host-owned registry for activations whose close may outlive Rivet's activation vars.
   *
   * The factory never creates a hidden fallback: omitting the owner would make an unconfirmed
   * SDK/database close unreachable after the framework clears vars.
   */
  readonly owner: HostResourceOwner;
  /** Required: the actor owns its database lifecycle and closes it on sleep. */
  readonly openDatabase: OpenChatDatabase;
  /** Required: read once per open to clear a record left by the previous activation. */
  readonly ownership: ChatOwnership;
  /** Deployment machine identity; every `open.workspace` URI must name this machine. */
  readonly machineId: string;
  /** Runtime plus default session init; a target may override session/lifecycle per open. */
  readonly chatConfig: ChatConfig;
  readonly options?: ActorOptionsInput;
  /** Observes wake and completed sleep/destroy teardown; failures go to `onDiagnostic`. */
  readonly onLifecycle?: (event: PeriActorLifecycleEvent) => void;
  readonly onDiagnostic?: (diagnostic: ChatActorDiagnostic) => void;
}

/** The SDK handle plus the activation-scoped resources acquired with it. */
export interface ChatActivation {
  readonly chat: ChatServiceContract;
  readonly database: ChatDatabase & { close(): Promise<void> };
}
