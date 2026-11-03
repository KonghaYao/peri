import type { ChildProcessWithoutNullStreams } from 'node:child_process';
import { SandboxError, type SandboxProfile } from '../instance/types.js';
import { MacProvider } from './mac-provider.js';
import { SystemdProvider } from './systemd-provider.js';

export interface ProviderStartOptions {
  sandboxId: string;
  runId: string;
  sandboxesRoot: string;
  sandboxRoot: string;
  workspacePath: string;
  homePath: string;
  policyPath: string;
  profile: SandboxProfile;
  periPath: string;
}

export interface ProviderHandle {
  readonly provider: 'macos-process-group' | 'systemd-user-unit';
  /** Transient systemd unit; null on macOS. */
  readonly unit: string | null;
  readonly process: ChildProcessWithoutNullStreams;
  /** Stop this unit/process group and confirm it exited. */
  stop(): Promise<void>;
}

export interface DoctorOptions {
  sandboxesRoot: string;
  periPath: string;
  periSha256: string;
  unitPrefix: string;
  profiles: Record<string, SandboxProfile>;
  startupTimeoutMs: number;
}

export interface Provider {
  readonly kind: 'macos-process-group' | 'systemd-user-unit';
  /** Environment check; the full isolation probe runs only when options are supplied. */
  doctor(options?: DoctorOptions): Promise<void>;
  start(options: ProviderStartOptions): Promise<ProviderHandle>;
  /** Return an active residual unit for this sandbox id, if any. */
  findActiveUnit?(sandboxId: string): Promise<string | null>;
  /** Doctor-only diagnostics; never used for automatic cleanup. */
  findResidualUnits?(): Promise<string[]>;
}

export interface ProviderOptions {
  unitPrefix?: string;
  stopTimeoutMs?: number;
}

export function providerFor(platform: NodeJS.Platform = process.platform, options: ProviderOptions = {}): Provider {
  if (platform === 'darwin') return new MacProvider(options);
  if (platform === 'linux') return new SystemdProvider(options);
  throw new SandboxError('unsupported_environment', `Unsupported platform: ${platform}`);
}
