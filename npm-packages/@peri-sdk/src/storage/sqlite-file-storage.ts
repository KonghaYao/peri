import { Database } from "bun:sqlite";
import { SESSION_LIST_SQL, sessionSummary, type SessionRow, type SessionSummary } from "./session-summary";
import type { SessionStorage } from "./types";

/** Configure Peri's SQLite Session Store at a local file path. */
export class SqliteFileStorage implements SessionStorage {
  constructor(private readonly options: { path: string }) {}

  deployment(): ReturnType<SessionStorage["deployment"]> {
    return { args: ["--session-store", this.options.path], env: {} };
  }

  async getSessions(cwd: string): Promise<SessionSummary[]> {
    const database = new Database(this.options.path, { readonly: true });
    try {
      return database.query<SessionRow, [string]>(SESSION_LIST_SQL).all(cwd).map(sessionSummary);
    } finally {
      database.close();
    }
  }
}
