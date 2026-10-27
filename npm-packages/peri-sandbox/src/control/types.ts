export type SandboxState =
  | 'stopped'
  | 'starting'
  | 'ready'
  | 'stopping'
  | 'failed'
  | 'stop_incomplete'
  | 'destroying'
  | 'deleted';
export type ErrorCode =
  | 'unsupported_environment'
  | 'startup_failed'
  | 'startup_cancelled'
  | 'interrupted'
  | 'not_running'
  | 'stop_incomplete'
  | 'conflict'
  | 'limit_exceeded'
  | 'deleted'
  | 'not_found';

export class ControlError extends Error {
  constructor(
    public readonly code: ErrorCode,
    message: string,
  ) {
    super(message);
  }
}

export interface SandboxRecord {
  id: string;
  name?: string;
  profile: string;
  state: SandboxState;
  desiredState: 'running' | 'stopped' | 'deleted';
  generation?: string;
  mcpUrl: string;
  lastError?: { code: string; message: string };
  createdAt: string;
  updatedAt: string;
}

export interface StoredSandbox extends SandboxRecord {
  policyJson: string;
  policyHash: string;
  resourceJson: string;
  appliedHash?: string;
  unit?: string;
}

export interface SandboxControl {
  create(spec: { name?: string; profile: string }, options: { requestId: string }): Promise<SandboxRecord>;
  start(id: string, options: { requestId: string }): Promise<SandboxRecord>;
  stop(id: string, options: { requestId: string }): Promise<SandboxRecord>;
  destroy(id: string, options: { requestId: string; deleteData: true }): Promise<SandboxRecord>;
  inspect(id: string): Promise<SandboxRecord>;
  list(): Promise<SandboxRecord[]>;
}
