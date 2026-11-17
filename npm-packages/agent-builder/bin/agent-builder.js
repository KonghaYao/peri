#!/usr/bin/env bun
/**
 * Packaged launcher. Bun is the runtime (Bun.serve + bun:sqlite), so `engines` names it explicitly.
 * The launcher only prepares private local directories and hands control to the bundled server;
 * it never reads or prints credentials.
 */
import { chmodSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { assertNoSymlinkAncestors, PrivatePathError } from './path-guard.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, '..');
const serverEntry = path.join(packageRoot, 'dist', 'server', 'main.js');

function usage() {
  console.log(`Peri Agent Builder — 本地单用户 Chat + Sandbox

用法:
  agent-builder start     启动同源 Web + API（默认）
  agent-builder doctor    检查部署配置（不发起 Store 调用）
  agent-builder --help
  agent-builder --version

环境变量见安装目录 README 与 .env.example。仅监听 127.0.0.1。`);
}

function version() {
  try {
    const manifest = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
    console.log(manifest.version);
  } catch {
    console.log('unknown');
  }
}

function prepareDirectories() {
  const cwd = process.cwd();
  const databasePath = process.env.DATABASE_PATH ?? '.data/agent-builder.db';
  const dataDir =
    databasePath === ':memory:' ? path.join(cwd, '.data') : path.dirname(path.resolve(cwd, databasePath));

  if (process.env.RIVETKIT_STORAGE_PATH === undefined || process.env.RIVETKIT_STORAGE_PATH.trim() === '') {
    process.env.RIVETKIT_STORAGE_PATH = path.join(dataDir, 'rivet');
  }
  const storage = path.resolve(cwd, process.env.RIVETKIT_STORAGE_PATH);

  // Validate both paths before the first mkdir so a rejected alias cannot be created through.
  assertNoSymlinkAncestors(dataDir);
  assertNoSymlinkAncestors(storage);
  mkdirSync(dataDir, { recursive: true, mode: 0o700 });

  const existed = existsSync(storage);
  mkdirSync(storage, { recursive: true, mode: 0o700 });
  // Never chmod a directory the user already owns; only the one this launcher created.
  if (!existed) chmodSync(storage, 0o700);
  process.env.RIVETKIT_STORAGE_PATH = storage;

  if (process.env.PERI_WEB_ROOT === undefined || process.env.PERI_WEB_ROOT.trim() === '') {
    process.env.PERI_WEB_ROOT = path.join(packageRoot, 'dist');
  }
}

async function main() {
  const command = process.argv[2] ?? 'start';
  if (command === '--help' || command === '-h' || command === 'help') {
    usage();
    return;
  }
  if (command === '--version' || command === '-v') {
    version();
    return;
  }
  if (command !== 'start' && command !== 'doctor') {
    console.error(`未知命令: ${command}`);
    usage();
    process.exitCode = 1;
    return;
  }
  if (!existsSync(serverEntry)) {
    console.error(`安装不完整：缺少 ${serverEntry}。请重新安装 tarball。`);
    process.exitCode = 1;
    return;
  }

  try {
    prepareDirectories();
  } catch (error) {
    if (!(error instanceof PrivatePathError)) throw error;
    console.error(
      '[agent-builder] 启动失败：DATABASE_PATH 或状态目录路径包含符号链接/非目录组件；请使用 realpath 后的真实路径',
    );
    process.exitCode = 1;
    return;
  }
  if (command === 'doctor') process.argv.push('--doctor');
  await import(serverEntry);
}

await main();
