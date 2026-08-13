import { describe, expect, it } from 'vitest';
import { connectionTransition } from './connection-state.mjs';

describe('connectionTransition', () => {
  it('keeps actions closed until the authoritative ready event', () => {
    expect(connectionTransition('connecting')).toMatchObject({ ready: false, busy: true });
    expect(connectionTransition('open')).toMatchObject({ ready: false, busy: true });
    expect(connectionTransition('ready')).toEqual({
      ready: true,
      busy: false,
      status: { text: '就绪', kind: 'ok' },
      problem: null,
    });
  });

  it('closes readiness throughout automatic reconnection', () => {
    expect(connectionTransition('reconnecting', { retryMs: 1499 })).toEqual({
      ready: false,
      busy: true,
      status: { text: '重连中（1s 后）', kind: 'warn' },
      problem: null,
    });
  });

  it('settles permanent and manual closure into one actionable truth', () => {
    expect(connectionTransition('fatal', { code: 4500 })).toMatchObject({
      ready: false,
      busy: false,
      problem: { action: 'reconnect', title: '本地 Agent 实例已离线' },
    });
    expect(connectionTransition('closed', {}, true)).toMatchObject({
      ready: false,
      busy: false,
      problem: { action: 'reconnect' },
    });
    expect(connectionTransition('closed', {}, false)?.problem).toBeNull();
  });

  it('does not overwrite presentation on heartbeat or unknown future events', () => {
    expect(connectionTransition('heartbeat')).toBeNull();
    expect(connectionTransition('future')).toBeNull();
  });
});
