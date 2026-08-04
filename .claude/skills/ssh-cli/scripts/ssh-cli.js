#!/usr/bin/env node
/**
 * ssh-cli — 通过系统 ssh 二进制操作远程机器的命令行工具
 * 单文件、零依赖（纯 Node 内置模块），由 ssh-cli skill 调用。
 *
 * 用法:
 *   node ssh-cli.js exec <host> "<command>" [--timeout <ms>] [--port <n>] [--key <path>]
 *   node ssh-cli.js read <host> <file_path> [--offset <n>] [--limit <n>]
 *   node ssh-cli.js write <host> <file_path> <content> [--append]   # 或 --stdin 从 stdin 读内容
 *   node ssh-cli.js edit <host> <file_path> <old_string> <new_string> [--all]
 *   node ssh-cli.js ls <host> <dir>
 *   node ssh-cli.js test <host>
 *
 * 全局选项（任意子命令后）:
 *   --hosts <list>     主机白名单（逗号分隔，'*' 放行所有）。不传默认放行
 *   --timeout <ms>     单次调用超时（默认 60000；0 = 不超时）
 *   --audit-log <path> 审计日志 JSONL（可选）
 *   --strict <mode>    known_hosts 策略: accept-new(默认) | ask | yes | no
 *   --port <n> / --key <path>  SSH 端口 / 私钥路径（可选）
 *
 * 环境变量: SSH_CLI_ALLOWED_HOSTS / SSH_CLI_TIMEOUT / SSH_CLI_AUDIT_LOG /
 *           SSH_CLI_STRICT_HOST_KEY
 *
 * 退出码: exec 远程命令非零退出时返回相同退出码；超时返回 124；
 *         其他错误返回 1。
 *
 * 凭据全部走系统 ssh（~/.ssh/config / agent / key），不进入命令行参数。
 */
'use strict';

const { spawn, spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const VERSION = '1.0.0';

// ─────────────────────────── 配置解析 ───────────────────────────

function parseGlobalArgs(argv) {
  const cfg = {
    hosts: null,          // null = 放行所有
    timeoutMs: 60000,
    auditLog: null,
    strict: 'accept-new',
    port: null,
    key: null,
    controlPath: path.join(
      os.tmpdir(),
      'ssh-cli-' + (process.getuid ? process.getuid() : 'user'),
    ),
  };
  const kv = (v) => (v === undefined ? undefined : String(v));
  const rest = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => (i + 1 < argv.length ? argv[++i] : undefined);
    switch (a) {
      case '--hosts': cfg.hosts = kv(next()); break;
      case '--timeout': cfg.timeoutMs = Number(next()) || cfg.timeoutMs; break;
      case '--audit': case '--audit-log': cfg.auditLog = kv(next()); break;
      case '--strict': cfg.strict = kv(next()) || cfg.strict; break;
      case '--port': cfg.port = Number(next()) || null; break;
      case '--key': cfg.key = kv(next()); break;
      case '--help': case '-h': printHelp(); process.exit(0);
      default:
        if (a.startsWith('-')) {
          process.stderr.write(`[ssh-cli] 未知参数: ${a}（--help 查看用法）\n`);
        } else {
          rest.push(a);
        }
    }
  }
  // 环境变量兜底
  if (cfg.hosts === null && process.env.SSH_CLI_ALLOWED_HOSTS) cfg.hosts = process.env.SSH_CLI_ALLOWED_HOSTS;
  if (process.env.SSH_CLI_TIMEOUT) cfg.timeoutMs = Number(process.env.SSH_CLI_TIMEOUT) || cfg.timeoutMs;
  if (!cfg.auditLog && process.env.SSH_CLI_AUDIT_LOG) cfg.auditLog = process.env.SSH_CLI_AUDIT_LOG;
  if (process.env.SSH_CLI_STRICT_HOST_KEY) cfg.strict = process.env.SSH_CLI_STRICT_HOST_KEY;
  if (process.env.SSH_CLI_PORT) cfg.port = Number(process.env.SSH_CLI_PORT) || null;
  if (!['accept-new', 'ask', 'yes', 'no'].includes(cfg.strict)) {
    process.stderr.write(`[ssh-cli] 警告: 非法 --strict 值 "${cfg.strict}"，回退 accept-new\n`);
    cfg.strict = 'accept-new';
  }
  return { cfg, rest };
}

function printHelp() {
  process.stderr.write(`ssh-cli v${VERSION} — 通过系统 ssh 操作远程机器（单文件、零依赖）

用法:
  ssh-cli exec <host> "<command>" [--timeout <ms>] [--port <n>] [--key <path>]
  ssh-cli read <host> <file_path> [--offset <n>] [--limit <n>]
  ssh-cli write <host> <file_path> <content> [--append]        # 或 --stdin
  ssh-cli edit <host> <file_path> <old_string> <new_string> [--all]
  ssh-cli ls <host> <dir>
  ssh-cli test <host>

全局选项:
  --hosts <list>     主机白名单（'*' 放行所有；不传默认放行）
  --timeout <ms>     单次调用超时（默认 60000；0 = 不超时）
  --audit-log <path> 审计日志 JSONL
  --strict <mode>    known_hosts 策略: accept-new(默认) | ask | yes | no
  --port <n>         SSH 端口
  --key <path>       SSH 私钥路径

环境变量: SSH_CLI_ALLOWED_HOSTS / SSH_CLI_TIMEOUT / SSH_CLI_AUDIT_LOG /
          SSH_CLI_STRICT_HOST_KEY / SSH_CLI_PORT

退出码: exec 远程命令非零退出返回同码；超时 124；其他错误 1。
`);
}

// ─────────────────────────── 小工具 ───────────────────────────

/** shell 单引号转义：保证字符串拼入远程命令后仍是一个字面量参数 */
function shellQuote(s) {
  return "'" + String(s).replace(/'/g, `'\\''`) + "'";
}

/** 主机白名单校验（cfg.hosts 为 null 时放行所有） */
function isHostAllowed(cfg, host) {
  if (cfg.hosts === null) return true;
  if (cfg.hosts === '*') return true;
  const raw = String(host || '');
  if (!raw) return false;
  const bare = raw.replace(/^.*@/, '').replace(/:\d+$/, '');
  return cfg.hosts.split(',').map((h) => h.trim()).filter(Boolean).some((allowed) => {
    if (allowed === raw || allowed === bare) return true;
    if (allowed.startsWith('.')) return bare.endsWith(allowed);
    return false;
  });
}

/** 审计日志：每行一条 JSON */
function audit(cfg, entry) {
  if (!cfg.auditLog) return;
  try {
    fs.appendFileSync(cfg.auditLog, JSON.stringify({ ts: new Date().toISOString(), ...entry }) + '\n');
  } catch (e) {
    process.stderr.write(`[ssh-cli] 审计日志写入失败: ${e.message}\n`);
  }
}

/** 探测 buffer 是否为二进制（含 NUL 字节） */
function isBinary(buf) {
  return buf.includes(0);
}

/** 构建基础 ssh 参数：连接复用 + 超时 + known_hosts 策略 */
function baseSshArgs(cfg, host) {
  const args = [
    '-o', 'ConnectTimeout=15',
    '-o', 'ServerAliveInterval=15',
    '-o', 'ServerAliveCountMax=3',
    '-o', `StrictHostKeyChecking=${cfg.strict}`,
    '-o', 'ControlMaster=auto',
    '-o', `ControlPath=${cfg.controlPath}/control-%C`,
    '-o', 'ControlPersist=600',
    '-o', 'BatchMode=yes',
  ];
  if (cfg.port) args.push('-p', String(cfg.port));
  if (cfg.key) args.push('-i', cfg.key);
  args.push(host);
  return args;
}

/**
 * 执行远程命令。
 * @returns {Promise<{stdout: Buffer, stderr: Buffer, exitCode: number, timedOut: boolean}>}
 */
function execRemote(cfg, host, cmd, { timeoutMs, stdin } = {}) {
  const t = timeoutMs ?? cfg.timeoutMs;
  // ssh 把参数空格拼接后交给远程 shell，整个命令必须 shellQuote 保证引号/管道正确
  const sshArgs = [...baseSshArgs(cfg, host), 'sh', '-c', shellQuote(cmd)];
  return new Promise((resolve, reject) => {
    let child;
    try {
      child = spawn('ssh', sshArgs, { stdio: ['pipe', 'pipe', 'pipe'] });
    } catch (e) {
      return reject(new Error(`无法启动 ssh 进程: ${e.message}`));
    }
    const stdout = [];
    const stderr = [];
    let timedOut = false;
    const timer = t > 0 ? setTimeout(() => {
      timedOut = true;
      try { child.kill('SIGKILL'); } catch { /* 已退出 */ }
    }, t) : null;
    child.stdout.on('data', (d) => stdout.push(d));
    child.stderr.on('data', (d) => stderr.push(d));
    child.on('error', (e) => {
      if (timer) clearTimeout(timer);
      reject(new Error(`ssh 进程错误: ${e.message}`));
    });
    child.on('close', (code) => {
      if (timer) clearTimeout(timer);
      resolve({
        stdout: Buffer.concat(stdout),
        stderr: Buffer.concat(stderr),
        exitCode: code,
        timedOut,
      });
    });
    if (stdin !== undefined) child.stdin.write(stdin);
    child.stdin.end();
  });
}

/** 白名单前置校验，拒绝时直接退出 */
function checkHost(cfg, host) {
  if (!host) fail('缺少 host 参数');
  if (!isHostAllowed(cfg, host)) {
    audit(cfg, { cmd: 'denied', host, decision: 'denied' });
    fail(`主机 "${host}" 不在白名单中。请加 --hosts "${host}" 或 SSH_CLI_ALLOWED_HOSTS（'*' 放行所有）`);
  }
}

function fail(msg) {
  process.stderr.write(`[ssh-cli] ${msg}\n`);
  process.exit(1);
}

// ─────────────────────────── 子命令 ───────────────────────────

async function cmdExec(cfg, args) {
  if (args.length < 2) fail('exec 需要 <host> "<command>"');
  const host = args.shift();
  checkHost(cfg, host);
  const command = args.join(' ');
  const t0 = Date.now();
  const r = await execRemote(cfg, host, command);
  audit(cfg, { cmd: 'exec', host, command, exitCode: r.exitCode, timedOut: r.timedOut, durationMs: Date.now() - t0 });
  if (r.timedOut) {
    process.stderr.write(`[ssh-cli] 超时: 命令超过时限未完成\n`);
    if (r.stdout.length) process.stdout.write(r.stdout);
    if (r.stderr.length) process.stderr.write(r.stderr);
    process.exit(124);
  }
  process.stdout.write(r.stdout);
  if (r.stderr.length) process.stderr.write(r.stderr);
  process.exit(r.exitCode ?? 1);
}

async function cmdRead(cfg, args) {
  if (args.length < 2) fail('read 需要 <host> <file_path>');
  const host = args.shift();
  const filePath = args.shift();
  checkHost(cfg, host);
  let off = 0, lim = null;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--offset') off = Number(args[i + 1]) || 0;
    if (args[i] === '--limit') lim = Number(args[i + 1]) ?? null;
  }
  if (off < 0) fail('offset 不能为负');
  const p = shellQuote(filePath);
  const sedRange = lim === null ? `${off + 1},$p` : (lim === 0 ? '1,0p' : `${off + 1},${off + lim}p`);
  const cmd = `sed -n ${shellQuote(sedRange)} ${p}; printf '\\000__LINES__\\000'; wc -l < ${p} 2>/dev/null || echo 0`;
  const r = await execRemote(cfg, host, cmd);
  if (r.timedOut) fail('读取超时');
  if (r.exitCode !== 0 && r.stdout.length === 0) {
    fail(`读取失败: ${r.stderr.toString('utf8').trim() || '(无输出)'}`);
  }
  const sep = Buffer.from('\x00__LINES__\x00');
  const idx = r.stdout.indexOf(sep);
  const contentBuf = idx >= 0 ? r.stdout.subarray(0, idx) : r.stdout;
  const linesBuf = idx >= 0 ? r.stdout.subarray(idx + sep.length) : Buffer.alloc(0);
  const total = parseInt(linesBuf.toString('utf8').trim(), 10) || null;
  if (isBinary(contentBuf) && contentBuf.length > 4096) fail('文件为二进制，仅支持文本');
  if (total !== null) process.stderr.write(`[ssh-cli] 总行数: ${total}，读取区间: 行 ${off + 1}${lim === null ? ' 起至末尾' : `-${off + lim}`}\n`);
  process.stdout.write(contentBuf);
  process.exit(r.exitCode ?? 0);
}

async function cmdWrite(cfg, args) {
  if (args.length < 2) fail('write 需要 <host> <file_path> <content>（或 --stdin）');
  const host = args.shift();
  const filePath = args.shift();
  checkHost(cfg, host);
  const append = args.includes('--append');
  const fromStdin = args.includes('--stdin');
  let content;
  if (fromStdin) {
    content = await new Promise((resolve) => {
      const chunks = [];
      process.stdin.on('data', (d) => chunks.push(d));
      process.stdin.on('end', () => resolve(Buffer.concat(chunks)));
    });
  } else {
    content = Buffer.from(args.join(' '), 'utf8');
  }
  const p = shellQuote(filePath);
  const cmd = `cat ${append ? '>>' : '>'} ${p} && wc -c < ${p}`;
  const t0 = Date.now();
  const r = await execRemote(cfg, host, cmd, { stdin: content });
  audit(cfg, { cmd: 'write', host, file_path: filePath, append, bytes: content.length, exitCode: r.exitCode, timedOut: r.timedOut });
  if (r.timedOut) fail('写入超时');
  if (r.exitCode !== 0) fail(`写入失败: ${r.stderr.toString('utf8').trim() || '(无输出)'}`);
  process.stdout.write(r.stdout);
  process.exit(0);
}

async function cmdEdit(cfg, args) {
  if (args.length < 4) fail('edit 需要 <host> <file_path> <old_string> <new_string> [--all]');
  const host = args.shift();
  const filePath = args.shift();
  const oldS = args.shift();
  const newS = args.shift();
  const replaceAll = args.includes('--all');
  checkHost(cfg, host);
  // 读全文（不截断）
  const readCmd = `cat ${shellQuote(filePath)} 2>&1; echo; echo '\\x00__EOF__\\x00'; wc -c < ${shellQuote(filePath)} 2>/dev/null || echo 0`;
  const readR = await execRemote(cfg, host, readCmd);
  if (readR.timedOut) fail('读取超时');
  if (readR.exitCode !== 0 && readR.stdout.length === 0) fail('读取失败: 文件不存在或不可读');
  const sep = Buffer.from('\x00__EOF__\x00');
  const idx = readR.stdout.indexOf(sep);
  const contentBuf = idx >= 0 ? readR.stdout.subarray(0, idx) : readR.stdout;
  if (isBinary(contentBuf)) fail('文件为二进制，仅支持文本');
  const body = contentBuf.toString('utf8');
  let body_, replacements;
  if (replaceAll) {
    const parts = body.split(oldS);
    if (parts.length === 1) fail('old_string 未在文件中找到');
    replacements = parts.length - 1;
    body_ = parts.join(newS);
  } else {
    const first = body.indexOf(oldS);
    if (first === -1) fail('old_string 未在文件中找到');
    const second = body.indexOf(oldS, first + oldS.length);
    if (second !== -1) fail(`old_string 出现多次（${countMatches(body, oldS)} 次），请加 --all 或提供更长的 old_string`);
    replacements = 1;
    body_ = body.slice(0, first) + newS + body.slice(first + oldS.length);
  }
  const writeR = await execRemote(cfg, host, `cat > ${shellQuote(filePath)}`, { stdin: body_ });
  if (writeR.timedOut) fail('写入超时');
  if (writeR.exitCode !== 0) fail('写入失败');
  audit(cfg, { cmd: 'edit', host, file_path: filePath, replacements });
  process.stdout.write(`替换完成: ${replacements} 处\n`);
  process.exit(0);
}

async function cmdLs(cfg, args) {
  if (args.length < 2) fail('ls 需要 <host> <dir>');
  const host = args.shift();
  const dir = args.shift();
  checkHost(cfg, host);
  const r = await execRemote(cfg, host, `ls -la ${shellQuote(dir)} 2>&1`);
  if (r.timedOut) fail('超时');
  if (r.exitCode !== 0) fail(`ls 失败: ${r.stderr.toString('utf8').trim()}`);
  process.stdout.write(r.stdout);
  process.exit(0);
}

async function cmdTest(cfg, args) {
  if (args.length < 1) fail('test 需要 <host>');
  const host = args.shift();
  checkHost(cfg, host);
  const cmd =
    `hostname; uname -srmo 2>/dev/null || uname -a; date -u '+%Y-%m-%dT%H:%M:%SZ'; ` +
    `echo "shell: $SHELL"; echo "home: $HOME"; echo "ssh-ok"`;
  const r = await execRemote(cfg, host, cmd);
  if (r.timedOut) fail('连接超时');
  if (r.exitCode !== 0) fail(`连接失败: ${r.stderr.toString('utf8').trim().split('\n')[0] || '认证失败'}`);
  process.stdout.write(`连接成功 → ${r.stdout.toString('utf8').trim()}\n`);
  audit(cfg, { cmd: 'test', host, exitCode: 0 });
  process.exit(0);
}

function countMatches(s, sub) {
  let n = 0, i = 0;
  while ((i = s.indexOf(sub, i)) !== -1) { n++; i += sub.length; }
  return n;
}

// ─────────────────────────── 入口 ───────────────────────────

async function main() {
  const { cfg, rest } = parseGlobalArgs(process.argv.slice(2));
  if (rest.length === 0) { printHelp(); process.exit(1); }
  try { fs.mkdirSync(cfg.controlPath, { recursive: true }); } catch { /* 忽略 */ }
  const sub = rest.shift();
  const args = rest;
  const subcommands = {
    exec: cmdExec, read: cmdRead, write: cmdWrite, edit: cmdEdit, ls: cmdLs, test: cmdTest,
  };
  if (!subcommands[sub]) fail(`未知子命令: ${sub}（支持 exec/read/write/edit/ls/test）`);
  try {
    await subcommands[sub](cfg, args);
  } catch (e) {
    process.stderr.write(`[ssh-cli] 执行异常: ${e.stack || e.message}\n`);
    process.exit(1);
  }
}

if (require.main === module) {
  main();
}

module.exports = { parseGlobalArgs, isHostAllowed, shellQuote, execRemote, VERSION };
