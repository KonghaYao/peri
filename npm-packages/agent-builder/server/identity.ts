import path from 'node:path';
import { createPrivateFileIfMissing } from './filesystem';

const MACHINE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function parseMachineId(value: string): string {
  const normalized = value.trim().toLowerCase();
  if (!MACHINE_ID.test(normalized)) {
    // The value is not echoed: it is deployment identity, and errors may reach logs.
    throw new Error('PERI_MACHINE_ID must be a lowercase UUID');
  }
  return normalized;
}

/**
 * Machine identity must survive restarts: chat ownership and sandbox URIs are keyed by it, so a
 * fresh random id per boot would orphan Store sessions and deny every previously authorized URI.
 */
export function loadOrCreateMachineId(
  identityDir: string,
  generate: () => string = () => crypto.randomUUID(),
): string {
  const file = path.join(identityDir, 'machine-id');
  const value = createPrivateFileIfMissing(file, `${generate()}\n`);
  return parseMachineId(value);
}
