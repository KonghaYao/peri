import { afterEach, describe, expect, it } from 'vitest';
import {
  acceptMessageDelivery,
  blockUnknownMessageDelivery,
  completeMessageDelivery,
  composerDraft,
  dismissFailedMessageDelivery,
  failMessageDelivery,
  markMessageDeliveryUncertain,
  messageSubmission,
  reconcileMessageProjection,
  resetMessageDelivery,
  retryMessageDelivery,
  setComposerDraft,
  startMessageDelivery,
} from './message-delivery';

afterEach(resetMessageDelivery);

describe('message delivery', () => {
  it('keeps an uncertain message in the outbox instead of duplicating it into the editor', () => {
    setComposerDraft('session-a', 'important work');
    startMessageDelivery('cmd-a', 'important work', 'session-a', 'chat-a');
    expect(composerDraft('session-a')).toBe('');
    markMessageDeliveryUncertain('cmd-a');
    expect(composerDraft('session-a')).toBe('');
    expect(messageSubmission()).toMatchObject({ phase: 'uncertain', retryable: true });
  });

  it('keeps a committed outbox item until its exact durable projection arrives', () => {
    startMessageDelivery('cmd-a', 'source', 'session-a', 'chat-a');
    acceptMessageDelivery('other');
    expect(messageSubmission()?.phase).toBe('sending');
    expect(completeMessageDelivery('other', 'committed')).toBe(false);
    expect(completeMessageDelivery('cmd-a', 'accepted')).toBe(false);
    expect(completeMessageDelivery('cmd-a', 'duplicate')).toBe(true);
    expect(messageSubmission()?.phase).toBe('committed');
    expect(reconcileMessageProjection(new Set(['other']))).toBe(false);
    expect(messageSubmission()?.commandId).toBe('cmd-a');
    expect(reconcileMessageProjection(new Set(['cmd-a']))).toBe(true);
    expect(messageSubmission()).toBeNull();
  });

  it('uses projection and terminal acknowledgement as independent release barriers', () => {
    startMessageDelivery('cmd-a', 'source', 'session-a', 'chat-a');
    expect(reconcileMessageProjection(new Set(['cmd-a']))).toBe(true);
    expect(messageSubmission()).toMatchObject({ commandId: 'cmd-a', projected: true });
    expect(completeMessageDelivery('cmd-a', 'committed')).toBe(true);
    expect(messageSubmission()).toBeNull();
  });

  it('does not restore a projected message when the terminal action reports an error', () => {
    startMessageDelivery('cmd-a', 'source', 'session-a', 'chat-a');
    reconcileMessageProjection(new Set(['cmd-a']));
    failMessageDelivery('cmd-a', 'agent rejected');
    expect(messageSubmission()).toBeNull();
    expect(composerDraft('session-a')).toBe('');
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

  it('restores a failed message only when the user explicitly returns to editing', () => {
    startMessageDelivery('cmd-failed', 'recover me', 'session-a', 'chat-a');
    failMessageDelivery('cmd-failed', 'rejected');
    expect(composerDraft('session-a')).toBe('');
    dismissFailedMessageDelivery();
    expect(composerDraft('session-a')).toBe('recover me');
  });

  it('never reconciles equal text without the exact command identity', () => {
    startMessageDelivery('cmd-a', 'same text', 'session-a', 'chat-a');
    completeMessageDelivery('cmd-a', 'committed');
    expect(reconcileMessageProjection(new Set())).toBe(false);
    expect(messageSubmission()).not.toBeNull();
  });

  it('preserves identity across a same-command retry', () => {
    startMessageDelivery('stable', 'source', 'session-a', 'chat-a');
    markMessageDeliveryUncertain('stable');
    retryMessageDelivery('stable');
    expect(messageSubmission()).toMatchObject({ commandId: 'stable', phase: 'sending', text: 'source' });
  });

  it('blocks server delivery-unknown without restoring or retrying the text', () => {
    startMessageDelivery('unknown', 'may have executed', 'session-a', 'chat-a');
    markMessageDeliveryUncertain('unknown');
    blockUnknownMessageDelivery('unknown');
    expect(messageSubmission()).toMatchObject({
      commandId: 'unknown', phase: 'delivery_unknown', retryable: false,
    });
    expect(composerDraft('session-a')).toBe('');
    dismissFailedMessageDelivery();
    expect(messageSubmission()?.phase).toBe('delivery_unknown');
    expect(composerDraft('session-a')).toBe('');
  });

  it('refuses to replace an unresolved delivery even when a caller forgets the guard', () => {
    expect(startMessageDelivery('first', 'one', 'session-a', 'chat-a')).toBe(true);
    expect(startMessageDelivery('second', 'two', 'session-b', 'chat-b')).toBe(false);
    expect(messageSubmission()).toMatchObject({ commandId: 'first', sessionId: 'session-a', text: 'one' });
  });
});
