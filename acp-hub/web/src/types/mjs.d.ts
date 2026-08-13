declare module '*.mjs' {
  export function beginOpen(commandId: string, sessionId: string, previousSessionId: string | null, previousChatId: string | null): { commandId: string; sessionId: string; previousSessionId: string | null; previousChatId: string | null };
  export function matchesOpening(opening: { commandId: string } | null, commandId?: string): boolean;
  export function terminalCanCommit(opening: { commandId: string } | null, ack: { commandId?: string; status?: string; chatId?: string }): boolean;
  export function shouldIgnoreLateAck(ids: Set<string>, ack: { commandId?: string; status?: string }): boolean;
  export function unimportedSessions<T extends { sessionId: string }>(sessions: T[], projectSessions: Array<{ acpSessionId?: string | null }>): T[];
  export function importCandidates<T extends { cwd?: string | null }>(sessions: T[], cwd: string): T[];
}
