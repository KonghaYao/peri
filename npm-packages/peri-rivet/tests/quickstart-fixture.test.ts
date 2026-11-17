import { describe, expect, test } from 'bun:test';
import type { ChatOpenResult } from '../src/rivet/index.js';
import { formatWorkspaceUri, parseWorkspaceUri } from '../src/rivet/index.js';
import { chatTarget, createChatHarness, createStubChatRuntime } from './support/chat-stub.js';

const CHAT_ID = 'rt_quickstart_1';
const MACHINE_ID = '00000000-0000-4000-8000-000000000001';

describe('quickstart fixture', () => {
  test('workspace URI pairs machineId with the runtime path', () => {
    const runtimePath = process.cwd();
    const workspaceUri = formatWorkspaceUri({ machineId: MACHINE_ID, runtimePath });
    const parsed = parseWorkspaceUri(workspaceUri);
    expect(parsed.machineId).toBe(MACHINE_ID);
    expect(parsed.runtimePath).toBe(runtimePath);
  });

  test('opens the quickstart target without a binding and reuses the activation', async () => {
    const harness = createChatHarness(createStubChatRuntime().runtime, CHAT_ID);

    const opened = (await harness.actions.open(harness.context, chatTarget(CHAT_ID))) as ChatOpenResult;
    expect(opened.chatId).toBe(CHAT_ID);
    expect(opened.workspace).toBe(chatTarget(CHAT_ID).workspace);
    expect(opened.binding).toBeNull();
    expect(opened.reused).toBe(false);

    const again = (await harness.actions.open(harness.context, chatTarget(CHAT_ID))) as ChatOpenResult;
    expect(again.reused).toBe(true);

    await expect(harness.actions.close(harness.context)).resolves.toEqual({ closed: true });
    expect(harness.databaseCloses()).toBe(1);
  });
});
