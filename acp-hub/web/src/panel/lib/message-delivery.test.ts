import { afterEach, describe, expect, it } from 'vitest';
import {
  acceptMessageDelivery,
  completeMessageDelivery,
  composerDraft,
  dismissFailedMessageDelivery,
  failMessageDelivery,
  markMessageDeliveryUncertain,
  messageSubmission,
  resetMessageDelivery,
  retryMessageDelivery,
  setComposerDraft,
  startMessageDelivery,
} from './message-delivery';

afterEach(resetMessageDelivery);

describe('message delivery', () => {
  it('owns draft removal and restores source text when delivery becomes uncertain', () => {
    setComposerDraft('session-a', 'important work');
    startMessageDelivery('cmd-a', 'important work', 'session-a', 'chat-a');
    expect(composerDraft('session-a')).toBe('');
    markMessageDeliveryUncertain('cmd-a');
    expect(composerDraft('session-a')).toBe('important work');
    expect(messageSubmission()).toMatchObject({ phase: 'uncertain', retryable: true });
  });

  it('ignores unrelated outcomes and completes only a matching terminal acknowledgement', () => {
    startMessageDelivery('cmd-a', 'source', 'session-a', 'chat-a');
    acceptMessageDelivery('other');
    expect(messageSubmission()?.phase).toBe('sending');
    expect(completeMessageDelivery('other', 'committed')).toBe(false);
    expect(completeMessageDelivery('cmd-a', 'accepted')).toBe(false);
    expect(completeMessageDelivery('cmd-a', 'duplicate')).toBe(true);
    expect(messageSubmission()).toBeNull();
  });

  it('isolates drafts by durable session and never overwrites newer user edits', () => {
    setComposerDraft('session-a', 'original');
    setComposerDraft('session-b', 'private b');
    startMessageDelivery('cmd-a', 'original', 'session-a', 'chat-a');
    setComposerDraft('session-a', 'newer edit');
    failMessageDelivery('cmd-a', 'rejected');
    expect(composerDraft('session-a')).toBe('newer edit');
    expect(composerDraft('session-b')).toBe('private b');
    dismissFailedMessageDelivery();
    expect(messageSubmission()).toBeNull();
  });

  it('preserves identity across a same-command retry', () => {
    startMessageDelivery('stable', 'source', 'session-a', 'chat-a');
    markMessageDeliveryUncertain('stable');
    retryMessageDelivery('stable');
    expect(messageSubmission()).toMatchObject({ commandId: 'stable', phase: 'sending', text: 'source' });
  });

  it('refuses to replace an unresolved delivery even when a caller forgets the guard', () => {
    expect(startMessageDelivery('first', 'one', 'session-a', 'chat-a')).toBe(true);
    expect(startMessageDelivery('second', 'two', 'session-b', 'chat-b')).toBe(false);
    expect(messageSubmission()).toMatchObject({ commandId: 'first', sessionId: 'session-a', text: 'one' });
  });
});
