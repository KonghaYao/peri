import { describe, expect, test } from 'bun:test';
import type { ChatDisplayState } from '../runtime';
import { readComposerMeta } from './composerMeta';

function state(session: ChatDisplayState['session']): ChatDisplayState {
  return {
    chat: { id: 'chat-1', title: 'Chat', updatedAt: '2026-10-20T00:00:00Z' },
    sessionId: 'chat-1', bindingRevision: 1, running: false, executionBlocked: false,
    readiness: 'ready', entries: [], toolCalls: [], turns: [],
    activeTurn: { turnId: null, status: null }, session,
    tasks: [], plans: { current: [], byTurn: {} }, interactions: [],
    pendingInteractions: [], inputQueue: null,
  };
}

const modelOption = {
  id: 'model', name: 'Model', type: 'select', category: 'model', currentValue: 'fast',
  options: [
    { value: 'fast', name: 'Fast' },
    { value: 'quality', name: 'Quality', description: '更慢但更强' },
  ],
};
const modeOption = {
  id: 'mode', name: 'Session Mode', type: 'select', category: 'mode', currentValue: 'accept_edit',
  options: [{ value: 'default', name: 'Default' }, { value: 'accept_edit', name: 'Accept Edit' }],
};
const effortOption = {
  id: 'thinking_effort', name: 'Thinking Effort', type: 'select', category: 'thought_level',
  currentValue: 'high', options: [{ value: 'low', name: 'Low' }, { value: 'high', name: 'High' }],
};

describe('readComposerMeta', () => {
  test('reads model, mode and thinking effort selectors from configOptions', () => {
    const meta = readComposerMeta(state({
      configOptions: [modeOption, modelOption, effortOption],
    }));
    expect(meta.model).toMatchObject({ id: 'model', label: '模型', currentLabel: 'Fast' });
    expect(meta.model?.options).toHaveLength(2);
    expect(meta.mode).toMatchObject({ id: 'mode', currentLabel: 'Accept Edit' });
    expect(meta.thinking).toMatchObject({ id: 'thinking_effort', currentLabel: 'High' });
    expect(meta.modelLabel).toBeUndefined();
    expect(meta.modeLabel).toBeUndefined();
  });

  test('falls back to session model/mode when configOptions are absent', () => {
    const meta = readComposerMeta(state({ model: 'claude-sonnet', mode: 'default' }));
    expect(meta.model).toBeUndefined();
    expect(meta.modelLabel).toBe('claude-sonnet');
    expect(meta.modeLabel).toBe('default');
  });

  test('ignores malformed options and still falls back', () => {
    const meta = readComposerMeta(state({
      model: 'fallback-model',
      configOptions: [
        { id: 'model', category: 'model', type: 'select', currentValue: 'broken', options: 'nope' },
        { id: '', category: 'mode', type: 'select', currentValue: 'x', options: [] },
        { id: 'mode', category: 'mode', type: 'boolean', currentValue: true },
      ],
    }));
    // options 不是数组时仍可读出当前值，但没有可切换选项。
    expect(meta.model).toMatchObject({ id: 'model', currentLabel: 'broken', options: [] });
    expect(meta.modeLabel).toBeUndefined();
    expect(meta.mode).toBeUndefined();
  });

  test('returns an empty meta for missing state', () => {
    expect(readComposerMeta(null)).toEqual({});
  });
});
