import { describe, expect, test } from 'bun:test';
import { createPeriChatActor } from '../src/rivet/chat-actor.js';
import type { ChatActorDiagnostic, ChatOwnership } from '../src/rivet/chat-ports.js';
import { createChatObservation } from './helpers.js';

function actor() {
  const ownership: ChatOwnership = {
    getItem: async () => null,
    setItem: async () => {},
    removeItem: async () => {},
  };
  return createPeriChatActor({
    openDatabase: () => ({
      listSessions: async () => [],
      getSession: async () => null,
      close: async () => {},
    }),
    ownership,
    machineId: 'machine_1',
    chatConfig: {
      runtime: {
        kind: 'transport',
        factory: () => {
          throw new Error('runtime is not used by this test');
        },
      },
    },
  });
}

function actionNames(definition: { readonly config: { readonly actions?: object } }): string[] {
  return Object.keys(definition.config.actions ?? {}).sort();
}

interface LifecycleHooks {
  onSleep(context: unknown): void | Promise<void>;
  onDestroy(context: unknown): void | Promise<void>;
}

interface FakeContext {
  readonly actorId: string;
  readonly key: string[];
  readonly keepAwake: <T>(promise: Promise<T>) => Promise<T>;
  readonly broadcast: (name: string, data: unknown) => void;
  vars: { chat: unknown; database: unknown; open: unknown };
}

function lifecycleContext(key: string[]): FakeContext {
  return {
    actorId: 'actor_chat',
    key,
    keepAwake: (untilReleased) => untilReleased,
    broadcast: () => {},
    vars: { chat: null, database: null, open: null },
  };
}

function lifecycleHooks(definition: { readonly config: unknown }): LifecycleHooks {
  return definition.config as LifecycleHooks;
}

describe('chat actor definition', () => {
  test('declares actor options on the definition, not on a registry', () => {
    const ownership: ChatOwnership = {
      getItem: async () => null,
      setItem: async () => {},
      removeItem: async () => {},
    };
    const definition = createPeriChatActor({
      openDatabase: () => ({
        listSessions: async () => [],
        getSession: async () => null,
        close: async () => {},
      }),
      ownership,
      machineId: 'machine_1',
      chatConfig: {
        runtime: {
          kind: 'transport',
          factory: () => {
            throw new Error('runtime is not used by this test');
          },
        },
      },
      options: { actionTimeout: 120_000 },
    });

    expect(definition.config.options.actionTimeout).toBe(120_000);
    expect(actionNames(definition)).toEqual([
      'activity',
      'close',
      'execute',
      'facts',
      'observe',
      'open',
      'readPayload',
    ]);
  });

  test('sleep and destroy on an unopened actor are no-ops that still report lifecycle', async () => {
    const diagnostics: ChatActorDiagnostic[] = [];
    const definition = actor();
    const hooks = lifecycleHooks(definition);
    const context = lifecycleContext(['session_1']);

    await expect(hooks.onSleep(context)).resolves.toBeUndefined();
    await expect(hooks.onDestroy(context)).resolves.toBeUndefined();
    expect(diagnostics).toHaveLength(0);
    expect(createChatObservation().phaseLimited).toBe(true);
  });
});
