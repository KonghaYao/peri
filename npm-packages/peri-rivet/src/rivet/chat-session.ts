import { createChatSession } from '@peri-code/sdk';
import { canonicalJson, type JsonObject } from '../json.js';
import type { ChatActivation, ChatActorDiagnostic, ChatActorOptions, ChatOpenTarget } from './chat-ports.js';
import { resetOwnership } from './ownership-reset.js';

/**
 * Opens one activation: clear the previous owner, then build the database and the chat handle.
 *
 * Order matters. The reset runs first because the SDK writes a new owner during construction, and
 * a database left open by a failed construction would hold a lock the next `open` cannot take.
 */
export async function openChatActivation(
  options: ChatActorOptions,
  target: ChatOpenTarget,
): Promise<ChatActivation> {
  try {
    await resetOwnership(options.ownership, target.chatId);
  } catch (error) {
    // A record that cannot be read or cleared must not be silently abandoned: fail this open so
    // the caller retries, instead of letting the SDK surface a misleading ownership conflict.
    diagnose(options, { type: 'ownership_reset_failed', error });
    throw error;
  }

  const database = options.openDatabase(target.storage);
  let chat: ChatActivation['chat'];
  try {
    chat = await createChatSession({
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
  } catch (error) {
    // Construction is all-or-nothing: the database must not outlive the handle that would use it.
    try {
      await database.close();
    } catch (closeError) {
      diagnose(options, { type: 'close_failed', scope: 'database', error: closeError });
    }
    throw error;
  }

  return { chat, database };
}

/**
 * Releases the chat handle and the database, in that order and independently.
 *
 * The database step runs even when the chat close fails: a leaked directory handle is worse than a
 * noisy failure report, and the actor keeps no state that a retry could resume from. Each failure
 * is reported once through `onDiagnostic`; `rethrow` lets an explicit `close` action surface
 * `close_incomplete`, while sleep/destroy only record the diagnostic.
 */
export async function closeChatActivation(
  options: ChatActorOptions,
  activation: Pick<ChatActivation, 'chat' | 'database'>,
  rethrow = true,
): Promise<void> {
  const failures: unknown[] = [];
  try {
    await activation.chat.close();
  } catch (error) {
    failures.push(error);
    diagnose(options, { type: 'close_failed', scope: 'chat', error });
  }
  try {
    await activation.database.close();
  } catch (error) {
    failures.push(error);
    diagnose(options, { type: 'close_failed', scope: 'database', error });
  }
  if (rethrow && failures.length > 0) {
    throw failures[0];
  }
}

/**
 * Identity that decides whether a repeated `open` may reuse the activation it already has.
 *
 * Only the chat, workspace and agent are compared: storage and lifecycle affect where data is
 * written, not which chat this handle is running, and re-running `open` with a new storage
 * endpoint must not silently replace a live activation.
 */
export function chatTargetFingerprint(target: ChatOpenTarget): string {
  return canonicalJson({
    chatId: target.chatId,
    workspace: {
      id: target.workspace.id,
      runtimePath: target.workspace.runtimePath,
      ...(target.workspace.hostPath === undefined ? {} : { hostPath: target.workspace.hostPath }),
    },
    agent: target.agent as JsonObject,
    ...(target.sessionPath === undefined ? {} : { sessionPath: target.sessionPath }),
  });
}

function diagnose(options: ChatActorOptions, diagnostic: ChatActorDiagnostic): void {
  try {
    options.onDiagnostic?.(diagnostic);
  } catch {
    // Diagnostics are observational; they must not change the outcome of open or close.
  }
}
