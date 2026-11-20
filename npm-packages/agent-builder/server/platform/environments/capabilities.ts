import { PlatformNotImplementedError } from '../errors';
import type { EnvironmentConfig, EnvironmentPackages } from './config';

function notExecuted(field: string, capability: string): PlatformNotImplementedError {
  return new PlatformNotImplementedError(`${capability} are not executed by this deployment`, field);
}

function hasPackages(packages: EnvironmentPackages): boolean {
  return packages.python.length > 0 || packages.system.length > 0 || packages.npm.length > 0;
}

export interface ExecutableEnvironmentOptions {
  /** The request (or an applied template) declared a network policy. */
  readonly networkRequested: boolean;
  readonly vaultIds: readonly string[];
}

/**
 * Rejects configurations whose capabilities this deployment cannot execute.
 *
 * Only the deployment workspace, inline files and the documented metadata actually run. Accepting
 * the rest and dropping it silently would make the response claim behavior that never happened, so
 * the first offending field fails the request and nothing is stored.
 */
export function assertExecutableEnvironment(
  config: EnvironmentConfig,
  options: ExecutableEnvironmentOptions,
): void {
  if (hasPackages(config.packages)) throw notExecuted('packages', 'Packages');
  if (config.setup_commands.length > 0) throw notExecuted('setup_commands', 'Setup commands');
  if (options.networkRequested) throw notExecuted('network.access', 'Network policies');
  if (config.desktop.enabled) throw notExecuted('desktop', 'Desktop provisioning');
  if (Object.keys(config.env).length > 0) throw notExecuted('env', 'Environment variables');
  if (config.skills.length > 0) throw notExecuted('skills', 'Skills');
  if (config.plugins.length > 0) throw notExecuted('plugins', 'Plugins');
  if (config.container_size !== null) throw notExecuted('container_size', 'Container sizes');
  if (options.vaultIds.length > 0) throw notExecuted('vault_ids', 'Vault attachments');
}
