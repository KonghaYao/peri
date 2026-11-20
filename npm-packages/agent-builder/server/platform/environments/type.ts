import { PlatformNotImplementedError, PlatformRequestError } from '../errors';

/** Hosting types the runtime can execute; everything else fails the request. */
export type ExecutableEnvironmentType = 'none' | 'self_hosted';

export const HOSTED_ENVIRONMENT_TYPE = 'openai_hosted';

/**
 * Resolves a requested hosting type to one this deployment executes.
 *
 * `self_hosted` and `none` keep the documented shape and run in the deployment workspace.
 * Hosted types are documented but cannot be provisioned here, so they answer `501` with the
 * offending field instead of being rewritten to a local environment. Values outside the
 * documented enum are rejected as invalid input.
 */
export function executableEnvironmentType(requested: string, param: string): ExecutableEnvironmentType {
  if (requested === 'self_hosted' || requested === 'none') return requested;
  if (requested === HOSTED_ENVIRONMENT_TYPE) {
    throw new PlatformNotImplementedError('Hosted environments are not available in this deployment', param);
  }
  throw new PlatformRequestError('Unknown environment type', param);
}
