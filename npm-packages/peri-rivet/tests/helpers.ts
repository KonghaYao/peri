import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { ChatObservation } from '@peri-code/sdk/observe';
import { type ActorError, isActorError, type KeepAwakePort } from '../src/index.js';

export interface RecordingKeepAwake {
  readonly port: KeepAwakePort;
  readonly active: number;
  readonly acquires: number;
  readonly releases: number;
}

export function createRecordingKeepAwake(): RecordingKeepAwake {
  let active = 0;
  let acquires = 0;
  let releases = 0;

  return {
    port: {
      acquire() {
        acquires += 1;
        active += 1;
        let released = false;
        return {
          release() {
            if (released) {
              return;
            }
            released = true;
            active -= 1;
            releases += 1;
          },
        };
      },
    },
    get active() {
      return active;
    },
    get acquires() {
      return acquires;
    },
    get releases() {
      return releases;
    },
  };
}

export interface TempDatabase {
  readonly path: string;
  cleanup(): void;
}

export function createTempDatabase(): TempDatabase {
  const directory = mkdtempSync(join(tmpdir(), 'peri-rivet-'));
  return {
    path: join(directory, 'resource.sqlite'),
    cleanup() {
      rmSync(directory, { recursive: true, force: true });
    },
  };
}

export function sleep(milliseconds: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, milliseconds);
  });
}

export function expectActorError(error: unknown): ActorError {
  if (!isActorError(error)) {
    throw new Error(`expected ActorError, received: ${String(error)}`);
  }
  return error;
}

export async function captureActorError(operation: Promise<unknown>): Promise<ActorError> {
  try {
    await operation;
  } catch (error) {
    return expectActorError(error);
  }
  throw new Error('expected operation to reject');
}

export function createChatObservation(overrides: Partial<ChatObservation> = {}): ChatObservation {
  return {
    chatId: 'chat_1',
    sessionId: 'session_1',
    bindingRevision: 1,
    phase: 'bound',
    phaseLimited: true,
    workspace: { id: 'ws_1', runtimePath: '/workspace' },
    instance: {
      kind: 'transport',
      state: 'live',
      running: false,
      pendingInputCount: 0,
      pendingInteractionCount: 0,
    },
    mcp: { source: 'unknown', servers: [] },
    ...overrides,
  };
}
