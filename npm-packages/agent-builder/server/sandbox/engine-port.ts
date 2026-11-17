import type { ChatEngine } from '../chats/engine';
import { AppError } from '../errors';
import type { SandboxActorPort, SandboxEnginePort } from './coordinator';

/**
 * Adapts the engine's optional sandbox actor to the coordinator's strict port. A deployment with
 * sandbox enabled but no registered actor fails loudly instead of silently running Chat-only.
 */
export function sandboxEnginePort(engine: ChatEngine): SandboxEnginePort {
  return {
    get endpoint(): string {
      const endpoint = engine.endpoint;
      if (endpoint === null) {
        throw new AppError('engine_unavailable', 'Rivet engine is not started', {
          details: { reason: 'sandbox_engine_missing' },
          retryable: true,
        });
      }
      return endpoint;
    },
    actor(workspaceUri: string): SandboxActorPort {
      const actor = engine.sandbox(workspaceUri);
      if (actor === null) {
        throw new AppError('engine_unavailable', 'Sandbox actor is not registered', {
          details: { reason: 'sandbox_engine_missing' },
          retryable: true,
        });
      }
      return actor;
    },
  };
}
