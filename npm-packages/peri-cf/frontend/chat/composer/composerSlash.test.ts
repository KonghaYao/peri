import { describe, expect, test } from 'bun:test';
import type { ChatDisplayState } from '../runtime';
import {
  applyComposerCommand, filterComposerCommands, findSlashTrigger, readAvailableCommands,
} from './composerSlash';

function state(commands: unknown): ChatDisplayState {
  return {
    chat: { id: 'chat-1', title: 'Chat', updatedAt: '2026-10-20T00:00:00Z' },
    sessionId: 'chat-1', bindingRevision: 1, running: false, executionBlocked: false,
    readiness: 'ready', entries: [], toolCalls: [], turns: [],
    activeTurn: { turnId: null, status: null },
    session: { availableCommands: commands as Array<Record<string, unknown>> },
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
    pendingInteractions: [], inputQueue: null,
  };
}

describe('slash triggers', () => {
  test('triggers only at line start with a non-whitespace query', () => {
    expect(findSlashTrigger('/', 1)).toEqual({ start: 0, end: 1, query: '' });
    expect(findSlashTrigger('/he', 3)).toEqual({ start: 0, end: 3, query: 'he' });
    expect(findSlashTrigger('hello /he', 9)).toBeNull();
    expect(findSlashTrigger('/hello world', 12)).toBeNull();
    expect(findSlashTrigger('line\n/he', 8)).toEqual({ start: 5, end: 8, query: 'he' });
    expect(findSlashTrigger('abc', 3)).toBeNull();
    expect(findSlashTrigger('/he', 1)).toEqual({ start: 0, end: 1, query: '' });
  });

  test('filters by name and description and caps the menu', () => {
    const commands = [
      { name: 'help', description: 'Show help' },
      { name: 'model', description: 'Switch model' },
      { name: 'plan', description: 'Plan mode' },
    ];
    expect(filterComposerCommands(commands, 'swi').map((item) => item.name)).toEqual(['model']);
    expect(filterComposerCommands(commands, 'switch').map((item) => item.name)).toEqual(['model']);
    expect(filterComposerCommands(commands, 'zzz')).toEqual([]);
    expect(filterComposerCommands(commands, '', 2)).toHaveLength(2);
  });

  test('replaces only the current query and keeps trailing text', () => {
    const trigger = findSlashTrigger('/he tail', 3)!;
    expect(applyComposerCommand('/he tail', trigger, 'help')).toEqual({
      text: '/help  tail', caret: 6,
    });
  });

  test('reads ACP commands and ignores malformed or duplicate entries', () => {
    expect(readAvailableCommands(state([
      { name: 'help', description: 'Show help', input: { hint: '<topic>' } },
      { name: 'help', description: 'duplicate' },
      { name: '', description: 'empty' },
      { name: 'bare' },
      null,
    ]))).toEqual([
      { name: 'help', description: 'Show help', hint: '<topic>' },
      { name: 'bare' },
    ]);
    expect(readAvailableCommands(null)).toEqual([]);
  });
});
