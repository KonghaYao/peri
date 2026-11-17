/**
 * Actor identity passed to product-provided locators. Deliberately excludes the Rivet context so
 * session lookup never depends on actor-runtime internals.
 */
export interface PeriActorLocator {
  readonly actorId: string;
  readonly key: readonly string[];
}
