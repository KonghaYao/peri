import { afterEach, describe, expect, it } from 'vitest';
import {
  acceptQuickStart,
  completeQuickStart,
  dismissFailedQuickStart,
  failQuickStart,
  markQuickStartUncertain,
  quickStartSubmission,
  resetQuickStart,
  retryQuickStartDelivery,
  settleLateQuickStart,
  startQuickStart,
} from './quick-start-delivery';

afterEach(resetQuickStart);

describe('quick start delivery', () => {
  it('preserves project, source text and command identity through retry', () => {
    expect(startQuickStart('create-1', 'project-1', 'first prompt')).toBe(true);
    markQuickStartUncertain('create-1');
    retryQuickStartDelivery('create-1');
    expect(quickStartSubmission()).toMatchObject({
      commandId: 'create-1', projectId: 'project-1', text: 'first prompt', phase: 'creating', retryable: true,
    });
  });

  it('returns one activation only for a complete matching terminal acknowledgement', () => {
    startQuickStart('create-1', 'project-1', 'first prompt');
    acceptQuickStart('other');
    expect(quickStartSubmission()?.phase).toBe('creating');
    expect(completeQuickStart('create-2', 'committed', 'session-1', 'chat-1')).toBeNull();
    expect(completeQuickStart('create-1', 'accepted', 'session-1', 'chat-1')).toBeNull();
    expect(completeQuickStart('create-1', 'committed', 'session-1', undefined)).toBeNull();
    expect(completeQuickStart('create-1', 'duplicate', 'session-1', 'chat-1')).toEqual({
      commandId: 'create-1', sessionId: 'session-1', chatId: 'chat-1', text: 'first prompt',
    });
    expect(quickStartSubmission()).toBeNull();
  });

  it('reconciles a late terminal acknowledgement without replaying the continuation', () => {
    startQuickStart('create-1', 'project-1', 'first prompt');
    markQuickStartUncertain('create-1');
    expect(settleLateQuickStart('create-1', 'committed', 'session-1', 'chat-1')).toBe(true);
    expect(quickStartSubmission()).toMatchObject({ phase: 'failed', retryable: false, text: 'first prompt' });
    dismissFailedQuickStart();
    expect(quickStartSubmission()).toBeNull();
  });

  it('refuses to overwrite unresolved work and ignores unrelated errors', () => {
    expect(startQuickStart('create-1', 'project-1', 'first prompt')).toBe(true);
    expect(startQuickStart('create-2', 'project-2', 'second prompt')).toBe(false);
    failQuickStart('other', 'wrong');
    expect(quickStartSubmission()).toMatchObject({ commandId: 'create-1', phase: 'creating' });
    failQuickStart('create-1', 'rejected');
    expect(quickStartSubmission()).toMatchObject({ phase: 'failed', detail: 'rejected', retryable: false });
  });
});
