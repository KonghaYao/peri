#!/usr/bin/env node
import { parseArgs, USAGE } from './args.js';
import { DatabaseFileError, verifyDatabaseFile } from './database-file.js';
import { startServer, type StartedServer } from './server.js';

function waitForSignal(): Promise<void> {
  return new Promise((resolve) => {
    let settled = false;
    const stop = () => {
      if (settled) return;
      settled = true;
      process.off('SIGINT', stop);
      process.off('SIGTERM', stop);
      resolve();
    };
    process.once('SIGINT', stop);
    process.once('SIGTERM', stop);
  });
}

async function main(): Promise<number> {
  const parsed = parseArgs(process.argv.slice(2));
  if (parsed.kind === 'help') {
    console.log(USAGE);
    return 0;
  }
  if (parsed.kind === 'error') {
    console.error(parsed.message);
    console.error(USAGE);
    return 2;
  }

  let databasePath: string;
  try {
    databasePath = await verifyDatabaseFile(parsed.databasePath);
  } catch (error) {
    console.error(error instanceof DatabaseFileError ? error.message : '数据库文件不可用');
    return 1;
  }

  let started: StartedServer | undefined;
  try {
    started = await startServer({ databasePath, port: parsed.port });
    console.log(`sqlite-mcp 已启动: ${started.url}`);
    await waitForSignal();
    return 0;
  } catch {
    console.error('无法启动 sqlite-mcp 服务');
    return 1;
  } finally {
    if (started !== undefined) await started.close().catch(() => undefined);
  }
}

void main()
  .then((code) => {
    process.exitCode = code;
  })
  .catch(() => {
    console.error('sqlite-mcp 运行失败');
    process.exitCode = 1;
  });
