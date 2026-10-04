import { connect } from "@tursodatabase/serverless";
import { SESSION_BY_ID_SQL, SESSION_LIST_SQL, sessionSummary, type SessionRow, type SessionSummary } from "./session-summary";
import type { SessionStorage } from "./types";

export class TursoStorage implements SessionStorage {
  constructor(
    private readonly options: {
      url: string;
      engine?: "turso" | "libsql";
      authToken?: string;
      tokenEnv?: string;
    },
  ) {}

  deployment(): ReturnType<SessionStorage["deployment"]> {
    const { url, engine = "turso", authToken, tokenEnv = "PERI_SDK_TURSO_AUTH_TOKEN" } =
      this.options;
    const args = ["--session-store", url, "--session-store-engine", engine];
    if (authToken || this.options.tokenEnv)
      args.push("--session-store-token-env", tokenEnv);
    return { args, env: authToken ? { [tokenEnv]: authToken } : {} };
  }

  async getSessions(cwd: string): Promise<SessionSummary[]> {
    const token = this.options.authToken ??
      (this.options.tokenEnv ? Bun.env[this.options.tokenEnv] : undefined);
    if (this.options.tokenEnv && !token)
      throw new Error("Session Storage credential is missing");
    const connection = connect({ url: this.options.url, authToken: token });
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
    const token = this.options.authToken ??
      (this.options.tokenEnv ? Bun.env[this.options.tokenEnv] : undefined);
    if (this.options.tokenEnv && !token)
      throw new Error("Session Storage credential is missing");
    const connection = connect({ url: this.options.url, authToken: token });
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
