import { ChatActionError, ChatCloseIncompleteError } from '@peri-code/sdk';
import { UserError } from 'rivetkit';
import { ZodError } from 'zod';
import { isPeriError } from '../contracts/errors.js';
import { isActorError } from '../errors.js';

/**
 * Bounded cause chain: only `name`/`message` cross the actor boundary, so arbitrary objects and
 * credentials inside a failure never reach the client.
 */
function describeFailure(error: unknown, depth = 0): string {
  if (!(error instanceof Error)) {
    return typeof error === 'string' ? error : 'non-error failure';
  }
  const cause =
    depth < 3 && error.cause !== undefined ? ` <- ${describeFailure(error.cause, depth + 1)}` : '';
  return `${error.name}: ${error.message}${cause}`;
}

/**
 * Rivet hides non-`UserError` failures behind `internal_error`.
 *
 * Only errors that already belong to a public Peri contract keep their code and message: the
 * package's own `ActorError`, the v5 `PeriError`, and the SDK's typed chat failures. Everything
 * else is rethrown so an unknown failure stays opaque instead of inventing a code callers cannot
 * trust.
 */
export function toUserError(error: unknown): never {
  if (isActorError(error)) {
    throw new UserError(error.message, { code: error.code, metadata: error.toJSON() });
  }
  if (isPeriError(error)) {
    throw new UserError(error.message, { code: error.code, metadata: error.toJSON() });
  }
  if (error instanceof ChatCloseIncompleteError) {
    throw new UserError(error.message, {
      code: 'close_incomplete',
      metadata: { incomplete: true, errors: error.failures.map((failure) => describeFailure(failure)) },
    });
  }
  if (error instanceof ZodError) {
    throw new UserError('invalid actor action input', {
      code: 'invalid_input',
      metadata: { issues: error.issues },
    });
  }
  if (error instanceof ChatActionError) {
    throw new UserError(error.message, {
      code: error.code,
      metadata: { requestId: error.requestId, disposition: error.disposition },
    });
  }
  throw error;
}
