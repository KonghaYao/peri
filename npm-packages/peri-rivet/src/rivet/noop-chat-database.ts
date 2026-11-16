import type { ChatDatabase } from '@peri-code/sdk';

/**
 * The smallest `ChatDatabase` the actor accepts: no sessions, nothing to close.
 *
 * Transport runtimes and examples still have to satisfy `ChatActorOptions.openDatabase`, but they
 * never read a Store; this keeps that shape out of every caller.
 */
export function createNoopChatDatabase(): ChatDatabase & { close(): Promise<void> } {
  return {
    listSessions: async () => [],
    getSession: async () => null,
    close: () => Promise.resolve(),
  };
}
