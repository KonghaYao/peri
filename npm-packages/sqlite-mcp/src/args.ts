import { DEFAULT_PORT } from './constants.js';

export const USAGE = [
  '用法:',
  '  sqlite-mcp <数据库路径> [--port <端口>]',
  '',
  '参数:',
  '  <数据库路径>   已存在的 SQLite 数据库文件；不会创建新文件',
  '  --port <端口>  监听端口，默认 31339；0 表示由系统分配',
  '  -h, --help     显示本帮助',
  '',
  '说明:',
  '  驱动可能在首次读写时切换到 WAL，并生成 <数据库>-wal/-shm 等侧车文件；',
  '  不保证与其他进程并发兼容。服务只监听 127.0.0.1，只应在可信本机使用。',
  '',
  '服务 MCP 端点为 /mcp。',
].join('\n');

export type ParsedArgs =
  | { kind: 'run'; databasePath: string; port: number }
  | { kind: 'help' }
  | { kind: 'error'; message: string };

function error(message: string): ParsedArgs {
  return { kind: 'error', message };
}

function parsePort(value: string): number | undefined {
  if (!/^\d+$/.test(value)) return undefined;
  const port = Number(value);
  return Number.isInteger(port) && port >= 0 && port <= 65535 ? port : undefined;
}

/** 解析 CLI 参数；只支持一个数据库路径，避免运行期切换目标。 */
export function parseArgs(argv: readonly string[]): ParsedArgs {
  if (argv.includes('--help') || argv.includes('-h')) return { kind: 'help' };

  let databasePath: string | undefined;
  let port = DEFAULT_PORT;
  let positionalOnly = false;

  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (arg === undefined) continue;

    if (!positionalOnly && arg === '--') {
      positionalOnly = true;
      continue;
    }
    if (!positionalOnly && (arg === '--port' || arg.startsWith('--port='))) {
      const raw = arg === '--port' ? argv[++index] : arg.slice('--port='.length);
      if (raw === undefined || raw === '') return error('--port 缺少端口值');
      const parsed = parsePort(raw);
      if (parsed === undefined) return error('端口必须是 0-65535 的整数');
      port = parsed;
      continue;
    }
    if (!positionalOnly && arg.startsWith('-') && arg !== '-') return error(`未知选项: ${arg}`);
    if (databasePath !== undefined) return error('只能提供一个数据库路径');
    databasePath = arg;
  }

  if (databasePath === undefined || databasePath === '') return error('缺少数据库路径');
  return { kind: 'run', databasePath, port };
}
