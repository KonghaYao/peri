import { describe, expect, test } from 'bun:test';
import { createPeriChatActor } from '../src/rivet/chat-actor.js';
import type { ChatActorOptions, ChatOwnership } from '../src/rivet/chat-ports.js';
import { createHostResourceOwner } from '../src/rivet/host-resource-owner.js';
import { createNoopChatDatabase } from '../src/rivet/noop-chat-database.js';
import { createStubChatRuntime } from './support/chat-stub.js';

const ownership: ChatOwnership = {
  getItem: async () => null,
  setItem: async () => {},
  removeItem: async () => {},
};

describe('createNoopChatDatabase', () => {
  test('reports no sessions and closes cleanly', async () => {
    const database = createNoopChatDatabase();
    expect(await database.listSessions()).toEqual([]);
    expect(await database.listSessions('/tmp/any-path')).toEqual([]);
    expect(await database.getSession('session_missing')).toBeNull();
    await expect(database.close()).resolves.toBeUndefined();
  });

  test('satisfies ChatActorOptions.openDatabase', () => {
    const options: ChatActorOptions = {
      owner: createHostResourceOwner(),
      openDatabase: createNoopChatDatabase,
      ownership,
      machineId: 'machine_noop',
      chatConfig: { runtime: createStubChatRuntime().runtime },
    };
    const definition = createPeriChatActor(options);
    expect(Object.keys(definition.config.actions ?? {})).toContain('open');
  });
});
