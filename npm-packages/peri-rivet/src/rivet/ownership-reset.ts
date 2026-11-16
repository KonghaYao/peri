import { clearSessionOwnership, readSessionOwnership } from '@peri-code/sdk';
import type { ChatOwnership } from './chat-ports.js';

/** What the reset found before deleting; `absent` must not be reported as a successful clear. */
export type OwnershipResetOutcome = 'absent' | 'cleared';

/**
 * Rivet fences one activation per actor key, so any ownership record found before `open` was
 * written by a previous activation of the same chat and is stale.
 *
 * Returns the previous owner so callers can log which instance was replaced. The SDK grows a
 * fresh owner per `createChatSession` call, so the record is always rewritten on the first bind.
 */
export async function resetOwnership(
  ownership: ChatOwnership,
  chatId: string,
): Promise<OwnershipResetOutcome> {
  const record = await readSessionOwnership(ownership, chatId);
  if (record === null) {
    return 'absent';
  }
  await clearSessionOwnership(ownership, chatId);
  return 'cleared';
}
