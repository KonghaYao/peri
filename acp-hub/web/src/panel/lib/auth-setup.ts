export interface AuthSetup {
  tokenFile: string;
  generateCommand: string;
}

/** Parse only the credential-free setup descriptor; ignore every other field. */
export function parseAuthSetup(value: unknown): AuthSetup | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const setup = (value as { setup?: unknown }).setup;
  if (!setup || typeof setup !== 'object' || Array.isArray(setup)) return null;
  const { tokenFile, generateCommand } = setup as Record<string, unknown>;
  if (typeof tokenFile !== 'string' || !tokenFile.trim()) return null;
  if (typeof generateCommand !== 'string' || !generateCommand.trim()) return null;
  return { tokenFile, generateCommand };
}
