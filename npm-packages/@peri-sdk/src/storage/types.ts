import type { SessionSummary } from "./session-summary";

/** Peri owns mutations; the SDK reads session metadata from the same Store with explicit credentials. */
export interface SessionStorage {
  getSessions(cwd: string): Promise<SessionSummary[]>;
  getSession(id: string): Promise<SessionSummary | null>;
}
