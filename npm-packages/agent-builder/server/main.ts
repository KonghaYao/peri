import { createHostResourceOwner } from '@peri-code/rivet/rivet';
import { AgentsRepository } from './agents/repository';
import { createApp } from './app';
import { createChatEngineWithStoreProbe, engineStartupDiagnostic } from './chats/engine';
import { SERVER_IDLE_TIMEOUT_SECONDS } from './chats/keepalive';
import { ChatsRepository } from './chats/repository';
import { ChatService } from './chats/service';
import { configFailure } from './config-error';
import { openDatabase, secureDatabaseFiles } from './db/database';
import { migrate } from './db/migrate';
import { runDoctor } from './doctor';
import { loadEnv } from './env';
import { diagnosticCode } from './errors';
import { startAutomationScheduler } from './platform/automations/runtime';
import { createSessionAutomationPort } from './platform/automations/session-port';
import { createPlatformRepositories } from './platform/repositories';
import { SessionRecorder } from './platform/sessions/recorder';
import { createSessionRuntime, installSessionRuntime, sessionDraftFor } from './platform/sessions/runtime';
import { seedProviderFromEnv } from './providers/env-provider';
import { SandboxCoordinator } from './sandbox/coordinator';
import { sandboxEnginePort } from './sandbox/engine-port';
import { createSandboxRuntime, type SandboxRuntimeHandle } from './sandbox/runtime';
import { createShutdown } from './shutdown';

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
    // The single Provider is deployment configuration: sync it from the environment before any
    // request can resolve it. Only the protocol type is logged; credentials never reach the log.
    const seeded = seedProviderFromEnv(db, env);
    if (seeded !== null) {
      console.log(`[agent-builder] provider: env ${seeded.type} (${seeded.action})`);
    }

    const sandbox = new SandboxCoordinator(env, sandboxEnginePort(engine));
    // The platform session surface needs the chat service and its recorder; both are installed
    // before any route can serve so a session request either runs or reports not implemented.
    const platformRepos = createPlatformRepositories(db);
    const recorder = new SessionRecorder({ repos: platformRepos });
    const chatService = new ChatService({
      env,
      engine,
      chats: new ChatsRepository(db),
      agents: new AgentsRepository(db),
      sandbox,
      draftFor: sessionDraftFor(platformRepos),
    });
    installSessionRuntime(createSessionRuntime({ repos: platformRepos, chats: chatService, recorder }));
    // Automations deliver through the installed session runtime; the scheduler stays in-process.
    const automations = startAutomationScheduler({
      repos: platformRepos,
      port: createSessionAutomationPort(platformRepos),
    });
    const interrupted = recorder.sweepInterrupted();
    if (interrupted.turns > 0 || interrupted.items > 0) {
      console.log(
        `[agent-builder] converged ${interrupted.turns} interrupted turn(s) and ${interrupted.items} open item(s)`,
      );
    }
    const app = createApp({ env, db, engine, chatService, sandbox });
    // Idle SSE chats must survive between heartbeats: the heartbeat interval is kept well below
    // this timeout (see `server/chats/keepalive.ts`).
    const server = Bun.serve({
      hostname: env.host,
      port: env.port,
      idleTimeout: SERVER_IDLE_TIMEOUT_SECONDS,
      fetch: app.fetch,
    });
    // `createShutdown` owns server and chat teardown; the scheduler hook rides on the same object
    // so the tick stops before the chat runtime and database are closed.
    const serverWithShutdown = server as typeof server & { stopAutomations?: () => void };
    serverWithShutdown.stopAutomations = () => {
      void automations.stop();
    };

    console.log(`[agent-builder] api listening on http://${server.hostname}:${server.port}`);
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
    // starting. A boot failure stays visible through the engine diagnostic while the process
    // survives; if the engine cannot bind its ports, rivetkit terminates the process, so the log
    // carries the actionable ports instead of a bare code.
    if (engine.status.mode === 'wasm') {
      void engine.start().catch(() => {
        console.error(
          '[agent-builder] chat engine failed to start; session routes will return 503 engine_unavailable',
          engineStartupDiagnostic(engine.health(), engine.status.reasons, env.enginePort),
        );
      });
    }

    const cleanup = createShutdown({
      stop: () => {
        (server as typeof server & { stopAutomations?: () => void }).stopAutomations?.();
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
      let complete = false;
      try {
        complete = (await cleanup.run()).complete;
      } catch (error) {
        // Ordered cleanup reports step failures itself; a throw here is unexpected (for example a
        // failing server stop) and must not skip the engine fallback below.
        console.error('[agent-builder] shutdown failed', { code: diagnosticCode(error) });
      }
      // The native engine is a child process and `process.exit()` does not reap it. Ordered
      // teardown may stop early (unconfirmed chats/host, or an engine timeout); the recorded
      // engine PID is the terminal fallback so a SIGTERM cannot leave our engine behind.
      const termination = await engine.forceStop?.();
      if (termination !== undefined && termination.status !== 'none') {
        console.error('[agent-builder] engine child cleanup', termination);
      }
      // A child that could not be signalled or read is still a cleanup failure, even when every
      // ordered step reported success.
      const engineCleanupFailed =
        termination !== undefined &&
        (termination.status === 'blocked' || termination.status === 'unreadable');
      secureDatabaseFiles(env.databasePath);
      db.close();
      process.exit(complete && !engineCleanupFailed ? 0 : 1);
    };

    // `on`, not `once`: while shutdown is running a second signal must not fall back to the
    // default handler, which would exit before the engine child is stopped.
    process.on('SIGINT', () => void shutdown('SIGINT'));
    process.on('SIGTERM', () => void shutdown('SIGTERM'));
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
    console.error('[agent-builder] failed to start', {
      code: diagnosticCode(error),
      hint: '检查环境变量格式（.env.example）、数据目录与端口占用后重启；可用 --doctor 查看部署诊断',
    });
  }
  process.exit(1);
}
