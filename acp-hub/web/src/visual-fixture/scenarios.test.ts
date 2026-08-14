import { afterEach, describe, expect, it } from 'vitest';
import { chatEntries, chatHead, connectionProblem, permissions, projects, projectSessions, promptRecovery, selectedCid, selectedSessionId } from '../panel/store';
import { principalRole } from '../panel/lib/auth-state';
import { messageSubmission } from '../panel/lib/message-delivery';
import * as store from '../panel/store';
import { DEFAULT_VISUAL_SCENARIO, installVisualScenario, resolveVisualScenario, VISUAL_NOW, visualScenarios } from './scenarios';

let dispose: (() => void) | null = null;
afterEach(() => { dispose?.(); dispose = null; });

describe('visual fixture scenarios', () => {
  it('falls back to the complete conversation and scopes its fixed clock', () => {
    const originalNow = Date.now;
    expect(resolveVisualScenario('unknown')).toBe(DEFAULT_VISUAL_SCENARIO);
    const installed = installVisualScenario('unknown');
    dispose = installed.dispose;
    expect(installed.scenario.id).toBe('conversation');
    expect(Date.now()).toBe(VISUAL_NOW);
    expect(projects().length).toBeGreaterThan(1);
    expect(selectedSessionId()).toBe('session-current');
    expect(selectedCid()).toBe('chat-current');
    expect(chatEntries().some((entry) => entry.text.includes('```rust'))).toBe(true);
    expect(chatEntries().flatMap((entry) => entry.toolCalls).some((tool) => tool.resultOmitted)).toBe(true);
    installed.dispose(); dispose = null;
    expect(Date.now).toBe(originalNow);
  });

  it('covers permissions, recovery uncertainty and closed-by-default read-only state', () => {
    expect(visualScenarios.map((item) => item.id)).toEqual(['catalog', 'conversation', 'permission-streaming', 'recovery-errors', 'terminal-readonly']);
    let installed = installVisualScenario('recovery-errors');
    expect(promptRecovery()?.evidenceIncomplete).toBe(true);
    expect(connectionProblem()?.action).toBe('login');
    expect(messageSubmission()?.phase).toBe('uncertain');
    installed.dispose();
    installed = installVisualScenario('terminal-readonly');
    dispose = installed.dispose;
    expect(principalRole()).toBe('read-only');
    expect(projectSessions().some((session) => !!session.archivedAt)).toBe(true);
  });

  it('contains only synthetic workspace facts and no credential material', () => {
    const installed = installVisualScenario('conversation');
    dispose = installed.dispose;
    const serialized = JSON.stringify({ projects: projects(), sessions: projectSessions(), entries: chatEntries() });
    expect(serialized).not.toMatch(/bearer|access[_ -]?token|cookie|tokens\.toml|\/Users\//i);
    expect(new Set(projectSessions().map((item) => item.id)).size).toBe(projectSessions().length);
  });

  it('always releases the fixture clock when scenario cleanup is repeated', () => {
    const hostNow = Date.now;
    const installed = installVisualScenario('catalog');
    installed.dispose();
    installed.dispose();
    expect(Date.now).toBe(hostNow);
  });

  it('releases the clock if fixture initialization throws', () => {
    const hostNow = Date.now;
    const originalReset = store.resetAuthenticatedSession;
    Object.defineProperty(store, 'resetAuthenticatedSession', { value: () => { throw new Error('fixture setup failed'); }, configurable: true });
    try {
      expect(() => installVisualScenario('catalog')).toThrow('fixture setup failed');
      expect(Date.now).toBe(hostNow);
    } finally {
      Object.defineProperty(store, 'resetAuthenticatedSession', { value: originalReset, configurable: true });
    }
  });

  it('installs and completely disposes every scenario without state leakage', () => {
    const hostNow = Date.now;
    for (const scenario of visualScenarios) {
      const installed = installVisualScenario(scenario.id);
      expect(projects().length).toBe(3);
      if (scenario.id === 'catalog') {
        expect(selectedSessionId()).toBeNull();
        expect(selectedCid()).toBeNull();
      }
      if (scenario.id === 'permission-streaming') {
        expect(chatHead()?.activeTurn?.turnStatus).toBe('awaitingPermission');
        expect(permissions()).toHaveLength(2);
        expect(chatEntries().some((entry) => entry.status === 'streaming')).toBe(true);
      }
      installed.dispose();
      expect(projects()).toEqual([]);
      expect(projectSessions()).toEqual([]);
      expect(selectedSessionId()).toBeNull();
      expect(selectedCid()).toBeNull();
      expect(Date.now).toBe(hostNow);
    }
  });
});
