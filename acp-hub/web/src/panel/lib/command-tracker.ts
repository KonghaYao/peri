export interface CommandFrame {
  commandId: string;
  [key: string]: unknown;
}

export interface CommandAck {
  commandId?: string;
  status?: string;
  [key: string]: unknown;
}

export interface CommandError {
  commandId?: string;
  code?: string;
  message?: string;
  retryable?: boolean;
}

export interface CommandCallbacks<A extends CommandAck = CommandAck, E extends CommandError = CommandError> {
  onAccepted?: (ack: A) => void;
  onTerminal?: (ack: A) => void;
  onError?: (error: E) => void;
  onUncertain?: (reason: 'timeout' | 'disconnect') => void;
  retryOnUncertain?: boolean;
}

export interface CommandRequest<F extends CommandFrame, A extends CommandAck = CommandAck, E extends CommandError = CommandError> {
  frame: F;
  label: string;
  callbacks?: CommandCallbacks<A, E>;
}

interface TrackedCommand<F extends CommandFrame, A extends CommandAck, E extends CommandError> extends CommandRequest<F, A, E> {
  timer: ReturnType<typeof setTimeout>;
}

export interface CommandTrackerOptions<F extends CommandFrame, A extends CommandAck, E extends CommandError> {
  timeoutMs: number;
  onFallbackUncertain: (request: CommandRequest<F, A, E>, reason: 'timeout' | 'disconnect') => void;
  onUncertainCountChange?: (count: number) => void;
}

export type DispatchResult = 'sent' | 'unavailable' | 'already_pending';

/**
 * Owns the browser command lifecycle from transport send through exactly one
 * terminal outcome. Accepted acknowledgements never release the command;
 * timeout or disconnect retain the original frame only when the caller opted
 * into same-command reconciliation.
 */
export class CommandTracker<
  F extends CommandFrame = CommandFrame,
  A extends CommandAck = CommandAck,
  E extends CommandError = CommandError,
> {
  private readonly pending = new Map<string, TrackedCommand<F, A, E>>();
  private readonly uncertain = new Map<string, CommandRequest<F, A, E>>();

  constructor(private readonly options: CommandTrackerOptions<F, A, E>) {}

  dispatch(request: CommandRequest<F, A, E>, send: (frame: F) => boolean): DispatchResult {
    const commandId = request.frame.commandId;
    if (this.pending.has(commandId)) return 'already_pending';
    if (!send(request.frame)) return 'unavailable';
    const timer = setTimeout(() => this.makeUncertain(commandId, 'timeout'), this.options.timeoutMs);
    this.pending.set(commandId, { ...request, timer });
    return 'sent';
  }

  acknowledge(ack: A): boolean {
    const commandId = ack.commandId;
    if (!commandId) return false;
    const tracked = this.pending.get(commandId);
    if (!tracked) {
      if (ack.status !== 'accepted') this.forget(commandId);
      return false;
    }
    if (ack.status === 'accepted') {
      tracked.callbacks?.onAccepted?.(ack);
      return true;
    }
    this.releasePending(commandId);
    this.forget(commandId);
    tracked.callbacks?.onTerminal?.(ack);
    return true;
  }

  fail(error: E): boolean {
    const commandId = error.commandId;
    if (!commandId) return false;
    const tracked = this.pending.get(commandId);
    if (!tracked) return false;
    this.releasePending(commandId);
    this.forget(commandId);
    tracked.callbacks?.onError?.(error);
    return true;
  }

  settleConnectionLoss(): void {
    for (const commandId of [...this.pending.keys()]) this.makeUncertain(commandId, 'disconnect');
  }

  retry(commandId: string, send: (frame: F) => boolean): DispatchResult | null {
    const request = this.uncertain.get(commandId);
    if (!request) return null;
    return this.dispatch(request, send);
  }

  forget(commandId: string): void {
    if (!this.uncertain.delete(commandId)) return;
    this.options.onUncertainCountChange?.(this.uncertain.size);
  }

  reset(): void {
    for (const tracked of this.pending.values()) clearTimeout(tracked.timer);
    this.pending.clear();
    this.uncertain.clear();
    this.options.onUncertainCountChange?.(0);
  }

  hasPending(commandId: string): boolean { return this.pending.has(commandId); }
  hasUncertain(commandId: string): boolean { return this.uncertain.has(commandId); }
  uncertainCount(): number { return this.uncertain.size; }

  private releasePending(commandId: string): TrackedCommand<F, A, E> | null {
    const tracked = this.pending.get(commandId);
    if (!tracked) return null;
    clearTimeout(tracked.timer);
    this.pending.delete(commandId);
    return tracked;
  }

  private makeUncertain(commandId: string, reason: 'timeout' | 'disconnect'): void {
    const tracked = this.releasePending(commandId);
    if (!tracked) return;
    const request: CommandRequest<F, A, E> = {
      frame: tracked.frame,
      label: tracked.label,
      callbacks: tracked.callbacks,
    };
    if (tracked.callbacks?.retryOnUncertain) {
      this.uncertain.set(commandId, request);
      this.options.onUncertainCountChange?.(this.uncertain.size);
    }
    if (tracked.callbacks?.onUncertain) tracked.callbacks.onUncertain(reason);
    else this.options.onFallbackUncertain(request, reason);
  }
}
