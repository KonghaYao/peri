import { randomBytes } from 'node:crypto';
import {
  chmodSync,
  closeSync,
  fsyncSync,
  lstatSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { basename, dirname, isAbsolute, join } from 'node:path';
import { z } from 'zod';
import { sandboxIdPattern } from '../instance/options.js';
import { SwarmError } from './errors.js';

/** Persisted identity of one logical environment; never contains the inner MCP token. */
export interface SwarmBinding {
  readonly sandboxId: string;
  readonly workspacePath: string;
  /** `null` while the environment is reserved and has never started. */
  readonly lastRunId: string | null;
  readonly lastUsedAt: string;
}

const bindingSchema = z.strictObject({
  sandboxId: z.string().regex(sandboxIdPattern),
  workspacePath: z.string().refine((value) => isAbsolute(value), 'must be an absolute path'),
  lastRunId: z.string().min(1).nullable(),
  lastUsedAt: z.string().refine((value) => !Number.isNaN(Date.parse(value)), 'must be an ISO timestamp'),
});

const bindingsFileSchema = z.strictObject({
  version: z.literal(1),
  bindings: z.record(z.string(), bindingSchema),
});

function parseBindings(path: string, text: string): Map<string, SwarmBinding> {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    throw new SwarmError('bindings_invalid', `bindings file is not valid JSON: ${path}`, { cause: error });
  }
  const parsed = bindingsFileSchema.safeParse(value);
  if (!parsed.success)
    throw new SwarmError('bindings_invalid', `bindings file does not match the expected schema: ${path}`, {
      cause: parsed.error,
    });
  return new Map(Object.entries(parsed.data.bindings));
}

function loadBindings(path: string): Map<string, SwarmBinding> {
  const entry = lstatSync(path, { throwIfNoEntry: false });
  if (entry === undefined) return new Map();
  if (entry.isSymbolicLink() || !entry.isFile())
    throw new SwarmError('bindings_invalid', `bindings path must be a regular file: ${path}`);
  if (entry.mode & 0o077) throw new SwarmError('bindings_insecure', `bindings file must be private (0600): ${path}`);
  let text: string;
  try {
    text = readFileSync(path, 'utf8');
  } catch (error) {
    throw new SwarmError('bindings_invalid', `cannot read bindings file: ${path}`, { cause: error });
  }
  return parseBindings(path, text);
}

/** Write through a private temp file so readers never observe a partial bindings file. */
function writeAtomic(path: string, text: string): void {
  const temporary = join(dirname(path), `.${basename(path)}.${process.pid}.${randomBytes(6).toString('hex')}.tmp`);
  let descriptor: number | undefined;
  try {
    descriptor = openSync(temporary, 'wx', 0o600);
    writeFileSync(descriptor, text);
    fsyncSync(descriptor);
    closeSync(descriptor);
    descriptor = undefined;
    renameSync(temporary, path);
    chmodSync(path, 0o600);
  } catch (error) {
    if (descriptor !== undefined) {
      try {
        closeSync(descriptor);
      } catch {
        // 关闭失败不改变主错误。
      }
    }
    try {
      unlinkSync(temporary);
    } catch {
      // 临时文件可能尚未创建或已 rename。
    }
    if (error instanceof SwarmError) throw error;
    throw new SwarmError('bindings_invalid', `cannot replace bindings file: ${path}`, { cause: error });
  }
}

/**
 * `<root>/bindings.json`: logical environment → sandbox identity.
 *
 * Only routing metadata is persisted. The inner MCP token stays in memory, so a
 * stolen bindings file cannot authenticate against a running sandbox.
 */
export class SwarmBindingsStore {
  readonly path: string;
  private readonly bindings: Map<string, SwarmBinding>;
  private queue: Promise<void> = Promise.resolve();

  constructor(root: string) {
    this.path = join(root, 'bindings.json');
    this.bindings = loadBindings(this.path);
  }

  get(logicalEnvId: string): SwarmBinding | undefined {
    return this.bindings.get(logicalEnvId);
  }

  record(logicalEnvId: string, binding: SwarmBinding): Promise<void> {
    this.bindings.set(logicalEnvId, binding);
    return this.flush();
  }

  forget(logicalEnvId: string): Promise<void> {
    if (!this.bindings.delete(logicalEnvId)) return this.queue;
    return this.flush();
  }

  private flush(): Promise<void> {
    const snapshot = `${JSON.stringify({ version: 1, bindings: Object.fromEntries(this.bindings) }, null, 2)}\n`;
    const write = this.queue.then(() => writeAtomic(this.path, snapshot));
    this.queue = write.then(
      () => undefined,
      () => undefined,
    );
    return write;
  }
}
