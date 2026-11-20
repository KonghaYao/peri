import type { Database } from 'bun:sqlite';
import type { AgentDraft } from '@shared/agent';
import type { ChatService } from '../../chats/service';
import type { PlatformRepositories } from '../repositories';
import { PlatformInputReceiptsRepository } from '../repositories/sessions';
import type { SessionRecorder } from './recorder';
import { readAgentSnapshot } from './serialize';

/**
 * Process-local wiring for the session family.
 *
 * The platform route table receives only the database handle, so the runtime that can actually
 * execute a session (chat engine + recorder) is installed by the process that owns it. A process
 * that never installs one keeps answering the scaffold `not_implemented` contract instead of
 * pretending a session can run.
 */
export interface SessionRuntime {
  readonly db: Database;
  readonly repos: PlatformRepositories;
  readonly chats: ChatService;
  readonly recorder: SessionRecorder;
  readonly receipts: PlatformInputReceiptsRepository;
}

const RUNTIMES = new WeakMap<Database, SessionRuntime>();

/** Installs (or replaces) the runtime for one database; returns a restore handle for tests. */
export function installSessionRuntime(runtime: SessionRuntime): () => void {
  const previous = RUNTIMES.get(runtime.db);
  RUNTIMES.set(runtime.db, runtime);
  return () => {
    if (previous === undefined) RUNTIMES.delete(runtime.db);
    else RUNTIMES.set(runtime.db, previous);
  };
}

export function sessionRuntimeFor(repos: PlatformRepositories): SessionRuntime | null {
  return RUNTIMES.get(repos.sessions.db) ?? null;
}

/**
 * Durable draft source for the chat service: inline platform agents persist their resolved draft
 * with the session, so a restarted process freezes the same target without the in-memory run.
 */
export function sessionDraftFor(repos: PlatformRepositories): (chatId: string) => AgentDraft | null {
  return (chatId) => {
    const row = repos.sessions.get(chatId);
    if (row === null) return null;
    return readAgentSnapshot(row.agent_config)?.draft ?? null;
  };
}

export function createSessionRuntime(options: {
  readonly repos: PlatformRepositories;
  readonly chats: ChatService;
  readonly recorder: SessionRecorder;
}): SessionRuntime {
  return {
    db: options.repos.sessions.db,
    repos: options.repos,
    chats: options.chats,
    recorder: options.recorder,
    receipts: new PlatformInputReceiptsRepository(options.repos.sessions.db),
  };
}
