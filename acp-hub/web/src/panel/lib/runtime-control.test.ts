import { afterEach, describe, expect, it } from 'vitest';
import {
  acceptRuntimeControl,
  confirmRuntimeControl,
  failRuntimeControl,
  markRuntimeControlUncertain,
  reconcileRuntimeControl,
  resetRuntimeControls,
  retryRuntimeControl,
  runtimeControlBusy,
  runtimeControlFor,
  startRuntimeControl,
} from './runtime-control';

afterEach(resetRuntimeControls);

describe('runtime control', () => {
  it('prevents conflicting controls for one runtime while keeping other chats independent', () => {
    expect(startRuntimeControl('cancel-1', 'chat-1', 'cancel')).toBe(true);
    expect(startRuntimeControl('close-1', 'chat-1', 'close')).toBe(false);
    expect(startRuntimeControl('close-2', 'chat-2', 'close')).toBe(true);
    expect(runtimeControlFor('chat-1')).toMatchObject({ kind: 'cancel', commandId: 'cancel-1' });
    expect(runtimeControlFor('chat-2')).toMatchObject({ kind: 'close', commandId: 'close-2' });
  });

  it('preserves command identity through uncertainty and same-request retry', () => {
    startRuntimeControl('cancel-1', 'chat-1', 'cancel');
    acceptRuntimeControl('cancel-1');
    markRuntimeControlUncertain('cancel-1');
    expect(runtimeControlFor('chat-1')).toMatchObject({ phase: 'uncertain', retryable: true });
    retryRuntimeControl('cancel-1');
    expect(runtimeControlFor('chat-1')).toMatchObject({ commandId: 'cancel-1', phase: 'sending' });
  });

  it('accepts only a matching terminal acknowledgement and waits for projection truth', () => {
    startRuntimeControl('close-1', 'chat-1', 'close');
    expect(confirmRuntimeControl('other', 'committed')).toBe(false);
    expect(confirmRuntimeControl('close-1', 'accepted')).toBe(false);
    expect(confirmRuntimeControl('close-1', 'duplicate')).toBe(true);
    expect(runtimeControlFor('chat-1')?.phase).toBe('confirmed');
    reconcileRuntimeControl('chat-1', false, false);
    expect(runtimeControlFor('chat-1')).not.toBeNull();
    reconcileRuntimeControl('chat-1', false, true);
    expect(runtimeControlFor('chat-1')).toBeNull();
  });

  it('releases cancel only when control projection proves the turn stopped', () => {
    startRuntimeControl('cancel-1', 'chat-1', 'cancel');
    confirmRuntimeControl('cancel-1', 'committed');
    reconcileRuntimeControl('chat-1', true, false);
    expect(runtimeControlBusy('chat-1', 'cancel')).toBe(true);
    reconcileRuntimeControl('chat-1', false, false);
    expect(runtimeControlFor('chat-1')).toBeNull();
  });

  it('allows a new attempt only after a definite failure', () => {
    startRuntimeControl('cancel-1', 'chat-1', 'cancel');
    failRuntimeControl('cancel-1', 'rejected');
    expect(runtimeControlBusy('chat-1')).toBe(false);
    expect(startRuntimeControl('cancel-2', 'chat-1', 'cancel')).toBe(true);
    expect(runtimeControlFor('chat-1')?.commandId).toBe('cancel-2');
  });
});
