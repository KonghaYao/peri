import { createHostResourceOwner } from '@peri-code/rivet/rivet';
import { AgentsRepository } from './agents/repository';
import { createApp } from './app';
import { createChatEngineWithStoreProbe } from './chats/engine';
import { SERVER_IDLE_TIMEOUT_SECONDS } from './chats/keepalive';
import { ChatsRepository } from './chats/repository';
import { ChatService } from './chats/service';
import { configFailure } from './config-error';
import { openDatabase, secureDatabaseFiles } from './db/database';
import { migrate } from './db/migrate';
import { runDoctor } from './doctor';
import { loadEnv } from './env';
import { diagnosticCode } from './errors';
import { SandboxCoordinator } from './sandbox/coordinator';
import { sandboxEnginePort } from './sandbox/engine-port';
import { createSandboxRuntime, type SandboxRuntimeHandle } from './sandbox/runtime';
import { createShutdown } from './shutdown';
import { findWebRoot } from './static';

async function main(): Promise<void> {
  const env = loadEnv();
  const owner = createHostResourceOwner();
  let sandboxRuntime: SandboxRuntimeHandle | null = null;
  if (env.sandbox.enabled) {
    sandboxRuntime = await createSandboxRuntime(env);
  }
  const sandboxHost =
    sandboxRuntime === null || env.sandbox.token === undefined
      ? null
      : {
          runtime: sandboxRuntime.runtime,
          token: env.sandbox.token,
          machineId: env.machineId,
          authorizedWorkspaces: [env.workspaceUri],
        };

  const engine = await createChatEngineWithStoreProbe(env, { owner, sandbox: sandboxHost });
  const db = openDatabase(env.databasePath);

  try {
    const schemaVersion = migrate(db);
    secureDatabaseFiles(env.databasePath);

    const sandbox = new SandboxCoordinator(env, sandboxEnginePort(engine));
    const chatService = new ChatService({
      env,
      engine,
      chats: new ChatsRepository(db),
      agents: new AgentsRepository(db),
      sandbox,
    });
    const webRoot = findWebRoot(env.webRoot);
    const app = createApp({ env, db, engine, chatService, sandbox, webRoot });
    // Idle SSE chats must survive between heartbeats: the heartbeat interval is kept well below
    // this timeout (see `server/chats/keepalive.ts`).
    const server = Bun.serve({
      hostname: env.host,
      port: env.port,
      idleTimeout: SERVER_IDLE_TIMEOUT_SECONDS,
      fetch: app.fetch,
    });

    console.log(`[agent-builder] api listening on http://${server.hostname}:${server.port}`);
    if (webRoot === null) {
      console.warn('[agent-builder] web assets not found; run `vite build` or install the tarball');
    } else {
      console.log(`[agent-builder] web root: ${webRoot}`);
    }
    console.log(`[agent-builder] sqlite schema v${schemaVersion} at ${env.databasePath}`);
    console.log(
      `[agent-builder] chat runtime: ${engine.status.mode}`,
      engine.status.mode === 'wasm'
        ? { enginePort: env.enginePort, rivetStoragePath: env.rivetStoragePath }
        : { reasons: engine.status.reasons },
    );
    console.log(`[agent-builder] sandbox: ${env.sandbox.mode}`, env.sandbox.reasons);
    console.log(`[agent-builder] machine: ${env.machineId}`);
    console.log(`[agent-builder] workspace: ${env.workspaceUri}`);

    // The API answers immediately; the first chat request waits for the engine if it is still
    // starting. A boot failure is reported through health/capabilities instead of killing the API.
    if (engine.status.mode === 'wasm') {
      void engine.start().catch(() => {
        console.error('[agent-builder] rivet engine is unavailable; see /api/health');
      });
    }

    const cleanup = createShutdown({
      stop: () => {
        server.stop(true);
      },
      chats: chatService,
      owner,
      sandbox: sandboxRuntime,
      engine,
    });
    let stopping = false;
    const shutdown = async (signal: string) => {
      if (stopping) return;
      stopping = true;
      console.log(`[agent-builder] ${signal} received, shutting down`);
      const report = await cleanup.run();
      secureDatabaseFiles(env.databasePath);
      db.close();
      process.exit(report.complete ? 0 : 1);
    };

    process.once('SIGINT', () => void shutdown('SIGINT'));
    process.once('SIGTERM', () => void shutdown('SIGTERM'));
  } catch (error) {
    db.close();
    throw error;
  }
}

if (process.argv.includes('--doctor')) {
  process.exit(runDoctor());
}

try {
  await main();
} catch (error) {
  const failure = configFailure(error);
  if (failure !== null) {
    console.error(`[agent-builder] failed to start: ${failure.hint}`, { code: failure.code });
  } else {
    console.error('[agent-builder] failed to start', { code: diagnosticCode(error) });
  }
  process.exit(1);
}
