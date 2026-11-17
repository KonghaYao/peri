/** One actor lifecycle transition, observed after the transition has taken effect. */
export type PeriActorLifecycleEvent = Readonly<{
  type: 'wake' | 'sleep' | 'destroy';
  actorId: string;
  key: readonly string[];
}>;

type LifecycleDiagnostic = Readonly<{ type: 'lifecycle_observer_failed'; error: unknown }>;

/**
 * Observers are diagnostic participants, not lifecycle participants: a throwing observer is
 * reported through the existing diagnostic channel instead of failing wake or teardown.
 */
export function notifyLifecycle(
  observer: ((event: PeriActorLifecycleEvent) => void) | undefined,
  event: PeriActorLifecycleEvent,
  onDiagnostic: ((diagnostic: LifecycleDiagnostic) => void) | undefined,
): void {
  if (observer === undefined) {
    return;
  }
  try {
    observer(event);
  } catch (error) {
    try {
      onDiagnostic?.({ type: 'lifecycle_observer_failed', error });
    } catch {
      // Diagnostics are observational; they must not affect lifecycle state.
    }
  }
}
