import postgres from "postgres";
import { DeadlineError, withDeadline } from "../util/deadline";
import {
  PostgresStorage as HttpStorage,
  openHttp, QUERY_TIMEOUT_MS, SET_DIRECTORY_SCOPE,
  type DirectoryConnection, type PostgresOptions,
} from "./postgres-storage";
import type { SessionRow } from "./session-summary";

function openNative({ url }: PostgresOptions): DirectoryConnection {
  const configured = Number(new URL(url).searchParams.get("connect_timeout"));
  const limit = QUERY_TIMEOUT_MS / 1000;
  const connectTimeout = configured > 0 ? Math.min(configured, limit) : limit;
  const connect = () => postgres(url, { max: 1, connect_timeout: connectTimeout });
  let client = connect();
  return {
    async all(query, param) {
      const read = client.begin("READ ONLY", async (transaction) => {
        await transaction.unsafe(SET_DIRECTORY_SCOPE);
        return transaction.unsafe<SessionRow[]>(query, [param]);
      });
      try {
        return await withDeadline(read, QUERY_TIMEOUT_MS, "PostgreSQL directory read");
      } catch (error) {
        if (error instanceof DeadlineError) {
          // A disconnected peer can stall even a server-side statement timeout.
          // Drop its socket before allowing the next read on this instance.
          await client.end({ timeout: 0 });
          client = connect();
        }
        throw error;
      }
    },
    close: () => client.end({ timeout: limit }),
  };
}

/** Uses the PostgreSQL wire protocol on Node/Bun, or the host's explicit HTTP endpoint. */
export class PostgresStorage extends HttpStorage {
  constructor(options: PostgresOptions) {
    super(options, options.httpEndpoint === undefined ? openNative : openHttp);
  }
}
