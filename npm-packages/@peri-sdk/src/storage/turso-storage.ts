import { connect } from "@tursodatabase/serverless";
import { SESSION_BY_ID_SQL, SESSION_LIST_SQL, sessionSummary, type SessionRow, type SessionSummary } from "./session-summary";
import type { SessionStorage } from "./types";

/** 显式传入的 Store 位置与凭证；不从宿主环境或 CLI 部署参数推断。 */
export class TursoStorage implements SessionStorage {
  constructor(private readonly options: { url: string; authToken?: string }) {}

  async getSessions(cwd: string): Promise<SessionSummary[]> {
    const connection = connect({ url: this.options.url, authToken: this.options.authToken });
    try {
      let rows: SessionRow[];
      try {
        rows = await connection.all(SESSION_LIST_SQL, cwd) as SessionRow[];
      } catch (error) {
        // A fresh Store has no schema until Peri opens its first Session.
        if (error instanceof Error && error.message.includes("no such table: threads")) return [];
        throw error;
      }
      return rows.map(sessionSummary);
    } finally {
      await connection.close();
    }
  }

  async getSession(id: string): Promise<SessionSummary | null> {
    const connection = connect({ url: this.options.url, authToken: this.options.authToken });
    try {
      let rows: SessionRow[];
      try {
        rows = await connection.all(SESSION_BY_ID_SQL, id) as SessionRow[];
      } catch (error) {
        if (error instanceof Error && error.message.includes("no such table: threads")) return null;
        throw error;
      }
      return rows[0] ? sessionSummary(rows[0]) : null;
    } finally {
      await connection.close();
    }
  }
}
