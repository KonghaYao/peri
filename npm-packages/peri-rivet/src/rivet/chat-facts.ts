import {
  type ChatFactEnvelope,
  type ChatService as ChatServiceContract,
  ChatViewReplayOverflowError,
} from '@peri-code/sdk';
import type { ChatFactsResult } from './chat-ports.js';

/**
 * Reads the current generation's full fact log from the SDK's synchronous subscribe replay.
 *
 * There is no cursor: the SDK replays every fact of the active binding, and a client that needs a
 * diff keeps its own `bindingRevision`/`sequence` position. A cursor would have to be persisted,
 * which this actor deliberately never does.
 */
export function readFacts(chat: ChatServiceContract): ChatFactsResult {
  const current = chat.binding;
  if (current === null) {
    return { binding: null, envelopes: [], overflow: false };
  }

  const captured: { binding: ChatFactsResult['binding']; replay: readonly ChatFactEnvelope[] | null } = {
    binding: current,
    replay: null,
  };
  let unsubscribe: (() => void) | undefined;
  try {
    unsubscribe = chat.subscribeFacts({
      onBinding: (binding, replay) => {
        captured.binding = binding;
        captured.replay = replay;
      },
      onFact: () => {
        // Live facts are delivered through the actor's own broadcast, not through this read.
      },
      onClosed: () => {},
    });
  } catch (error) {
    if (error instanceof ChatViewReplayOverflowError) {
      return { binding: current, envelopes: [], overflow: true };
    }
    throw error;
  } finally {
    unsubscribe?.();
  }

  const binding = captured.binding ?? current;
  return { binding, envelopes: [...(captured.replay ?? [])], overflow: false };
}
