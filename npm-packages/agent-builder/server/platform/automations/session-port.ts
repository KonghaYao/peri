import type { PlatformRepositories } from '../repositories';
import type { CreateSessionParams } from '../sessions/params';
import { sessionRuntimeFor } from '../sessions/runtime';
import { SessionService } from '../sessions/service';

/** Narrow seam between automations and the session family; automations never speak to a chat actor. */
export interface AutomationSessionCreateInput {
  readonly environment: CreateSessionParams['environment'];
  readonly input: CreateSessionParams['input'];
  readonly agent_id?: string;
  readonly agent?: CreateSessionParams['agent'];
  readonly vault_ids?: CreateSessionParams['vault_ids'];
  readonly metadata?: CreateSessionParams['metadata'];
}

export interface AutomationSessionPort {
  create(input: AutomationSessionCreateInput): Promise<{ readonly sessionId: string }>;
  send(sessionId: string, text: string): Promise<void>;
}

/**
 * Production adapter over `SessionService`. It delegates creation, agent resolution and input
 * submission instead of reimplementing session lifecycle rules inside the automation family.
 */
export function createSessionAutomationPort(repos: PlatformRepositories): AutomationSessionPort {
  const service = (): SessionService => {
    const runtime = sessionRuntimeFor(repos);
    if (runtime === null) {
      throw new Error('Session runtime is not installed');
    }
    return new SessionService(runtime);
  };

  return {
    async create(input) {
      const params: CreateSessionParams = {
        environment: input.environment,
        ...(input.input === undefined ? {} : { input: input.input }),
        ...(input.agent_id === undefined ? {} : { agent_id: input.agent_id }),
        ...(input.agent === undefined ? {} : { agent: input.agent }),
        ...(input.vault_ids === undefined ? {} : { vault_ids: input.vault_ids }),
        ...(input.metadata === undefined ? {} : { metadata: input.metadata }),
      };
      const created = await service().create(params);
      return { sessionId: created.session.id };
    },

    async send(sessionId, text) {
      const message = {
        type: 'agent.session.input.message' as const,
        input: [{ role: 'user' as const, content: [{ type: 'input_text', text }] }],
      };
      await service().submitInputs(sessionId, { events: [message] }, null);
    },
  };
}
