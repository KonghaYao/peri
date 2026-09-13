import { createApp, MemCache } from "./app.js";
import type { ViewerDataSource } from "./routes/api.js";
import { ViewerDataAdapter } from "./data_adapter.js";

export interface ViewerConfig {
  host: string;
  port: number;
  dbPath: string;
}

export function loadConfig(env: Record<string, string | undefined> = process.env): ViewerConfig {
  const port = Number(env.PERI_DB_VIEWER_PORT ?? env.PORT ?? "8741");
  if (!Number.isInteger(port) || port < 1 || port > 65_535) throw new Error("PERI_DB_VIEWER_PORT must be between 1 and 65535");
  return {
    // The viewer is a local diagnostic surface; binding is deliberately not configurable.
    host: "127.0.0.1",
    port,
    dbPath: env.PERI_DB_PATH ?? env.PERI_THREADS_DB ?? "~/.peri/threads/threads.db",
  };
}

export interface RunningViewer {
  stop(): void;
}

/** Start an already-open read-only data source. The caller owns database construction and cleanup. */
export function startServer(dataSource: ViewerDataSource, config: ViewerConfig = loadConfig()): RunningViewer {
  const cache = new MemCache();
  cache.startCleanup();
  let server: ReturnType<typeof Bun.serve>;
  try {
    const app = createApp(dataSource, { cache });
    server = Bun.serve({ hostname: config.host, port: config.port, fetch: app.fetch });
  } catch (error) {
    cache.stopCleanup();
    dataSource.close();
    throw error;
  }
  console.log(`Peri DB Viewer running at http://${config.host}:${server.port}`);
  let stopped = false;
  return {
    stop() {
      if (stopped) return;
      stopped = true;
      server.stop(true);
      cache.stopCleanup();
      dataSource.close();
    },
  };
}

export { createApp } from "./app.js";

if (import.meta.main) {
  try {
    const config = loadConfig();
    const dataSource = await ViewerDataAdapter.open(config.dbPath);
    startServer(dataSource, config);
  } catch (error) {
    console.error(`Peri DB Viewer failed to start: ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
