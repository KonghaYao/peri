import { type KeepAwakePort, promiseKeepAwakePort } from '../host/keep-awake.js';

/**
 * The part of a Rivet action/wake context needed to hold an actor awake.
 *
 * Kept structural so the same bridge works for action, wake and run contexts without leaking a
 * concrete Rivet generic through the Peri-facing host API.
 */
export interface RivetKeepAwakeContext {
  keepAwake<T>(promise: Promise<T>): Promise<T>;
}

export interface RivetKeepAwakeOptions {
  /** Receives a rejected `keepAwake` promise; the coordinator retries on the next activity change. */
  readonly onError?: (error: unknown) => void;
}

/**
 * Bridges the activity coordinator to Rivet's `keepAwake(deferred.promise)` primitive.
 *
 * One acquired hold maps to one Rivet keep-awake promise, held for the whole active epoch and
 * released only after the host has drained accepted work.
 */
export function rivetKeepAwakePort(
  context: RivetKeepAwakeContext,
  options: RivetKeepAwakeOptions = {},
): KeepAwakePort {
  return promiseKeepAwakePort((untilReleased) => {
    void context.keepAwake(untilReleased).catch((error: unknown) => {
      options.onError?.(error);
    });
  });
}
