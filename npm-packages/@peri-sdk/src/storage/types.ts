import type { SessionSummary } from "./session-summary";

/** Peri owns mutations; the SDK reads session metadata from the same Store. */
export interface SessionStorage {
  deployment(): {
    args: string[];
    env: Record<string, string>;
  };
  getSessions(cwd: string): Promise<SessionSummary[]>;
}
