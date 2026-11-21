#!/usr/bin/env node
// Regenerate the cold workspace tool snapshot from the real provider binary.
//
// The snapshot must describe the tools that `mcp-packages/workspace` serves in
// this checkout. Instead of parsing Rust sources, this script starts the
// provider the same way the sandbox backend does
// (`peri mcp-start peri --capability workspace --stdio`) against a throwaway
// workspace and records the `tools/list` result. Nothing is sandboxed and no
// SRT process is involved.
//
// Usage:
//   node scripts/generate-workspace-tool-snapshot.mjs [--peri <path>] [--check]
//
// Binary resolution order: `--peri`, `PERI_SNAPSHOT_PERI_BIN`, the repository
// debug build (`target/debug/peri`), the repository release build, then `peri`
// on PATH.

import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repositoryRoot = resolve(packageRoot, '..', '..');
const outputPath = join(packageRoot, 'src', 'swarm', 'cold-tools.generated.ts');
const protocolVersion = '2026-07-28';
const providerCommand = 'peri mcp-start peri --capability workspace --stdio';

function parseArgs(argv) {
  const options = { check: false, periPath: undefined };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--check') options.check = true;
    else if (argument === '--peri') {
      options.periPath = argv[index + 1];
      index += 1;
      if (options.periPath === undefined) throw new Error('--peri requires a path');
    } else if (argument === '--help' || argument === '-h') {
      console.log('usage: node scripts/generate-workspace-tool-snapshot.mjs [--peri <path>] [--check]');
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${argument}`);
    }
  }
  return options;
}

function resolvePeriBinary(explicitPath) {
  const candidates = [
    explicitPath,
    process.env.PERI_SNAPSHOT_PERI_BIN,
    join(repositoryRoot, 'target', 'debug', 'peri'),
    join(repositoryRoot, 'target', 'release', 'peri'),
  ].filter((candidate) => candidate !== undefined && candidate.length > 0);
  for (const candidate of candidates) {
    if (existsSync(candidate)) return resolve(candidate);
  }
  try {
    const fromPath = execFileSync('which', ['peri'], { encoding: 'utf8' }).trim();
    if (fromPath.length > 0) return fromPath;
  } catch {
    // 没有 PATH 条目时继续给出构建提示。
  }
  throw new Error(
    'no peri binary found; build one with ./scripts/cargo-rmcp-patched.sh build -p peri-tui --bin peri ' +
      'or pass --peri <path>',
  );
}

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map((item) => canonicalJson(item)).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    const entries = Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`);
    return `{${entries.join(',')}}`;
  }
  return JSON.stringify(value);
}

function digestOf(tools) {
  return `sha256:${createHash('sha256').update(canonicalJson(tools)).digest('hex')}`;
}

/**
 * Minimal newline-delimited JSON-RPC client for `mcp-start --stdio`.
 *
 * Returns the `initialize` serverInfo and the real `tools/list` result.
 */
function requestToolsList(periPath) {
  return new Promise((resolvePromise, reject) => {
    const workspace = mkdtempSync(join(tmpdir(), 'peri-tool-snapshot-'));
    const child = spawn(
      periPath,
      ['mcp-start', 'peri', '--capability', 'workspace', '--stdio', '--workspace', workspace],
      { stdio: ['pipe', 'pipe', 'pipe'] },
    );
    let stdout = '';
    let stderr = '';
    let serverInfo;
    let settled = false;
    const timer = setTimeout(() => {
      finish(new Error(`timed out after 30s; stderr tail:\n${stderr.slice(-2000)}`));
    }, 30_000);
    timer.unref();

    function cleanup() {
      clearTimeout(timer);
      rmSync(workspace, { recursive: true, force: true });
      if (!child.killed) child.kill('SIGKILL');
    }
    function finish(error, value) {
      if (settled) return;
      settled = true;
      cleanup();
      if (error === undefined) resolvePromise(value);
      else reject(error);
    }
    function send(message) {
      child.stdin.write(`${JSON.stringify(message)}\n`);
    }

    child.once('error', (error) => finish(error));
    child.once('exit', (code, signal) => {
      if (!settled)
        finish(
          new Error(
            `peri exited before tools/list (code=${code}, signal=${signal}); stderr tail:\n${stderr.slice(-2000)}`,
          ),
        );
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString('utf8');
    });
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString('utf8');
      for (let newline = stdout.indexOf('\n'); newline >= 0; newline = stdout.indexOf('\n')) {
        const line = stdout.slice(0, newline).trim();
        stdout = stdout.slice(newline + 1);
        if (line.length === 0) continue;
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          continue;
        }
        if (message.id === 1 && message.error !== undefined) {
          finish(new Error(`initialize failed: ${JSON.stringify(message.error)}`));
        } else if (message.id === 1 && message.result !== undefined) {
          serverInfo = message.result.serverInfo;
          send({ jsonrpc: '2.0', method: 'notifications/initialized' });
          send({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
        } else if (message.id === 2) {
          if (message.error !== undefined) finish(new Error(`tools/list failed: ${JSON.stringify(message.error)}`));
          else finish(undefined, { serverInfo, tools: message.result?.tools });
        }
      }
    });

    send({
      jsonrpc: '2.0',
      id: 1,
      method: 'initialize',
      params: {
        protocolVersion,
        capabilities: {},
        clientInfo: { name: 'peri-sandbox-tool-snapshot', version: '0.0.0' },
      },
    });
  });
}

function normalizeTool(tool) {
  const name = tool?.name;
  const description = tool?.description;
  const inputSchema = tool?.inputSchema;
  if (typeof name !== 'string' || name.length === 0) throw new Error(`tool has no name: ${JSON.stringify(tool)}`);
  if (typeof description !== 'string' || description.trim().length === 0)
    throw new Error(`tool ${name} has no description`);
  if (inputSchema === null || typeof inputSchema !== 'object' || Array.isArray(inputSchema))
    throw new Error(`tool ${name} has no inputSchema object`);
  return { name, description, inputSchema };
}

function renderSource(snapshot) {
  return [
    '// biome-ignore-all format: generated workspace tool snapshot',
    '// Generated by scripts/generate-workspace-tool-snapshot.mjs. Do not edit by hand.',
    `// Source: \`${providerCommand}\` (peri-mcp-workspace, providerVersion ${snapshot.providerVersion}).`,
    '',
    `export const COLD_WORKSPACE_TOOL_SNAPSHOT = ${JSON.stringify(snapshot, null, 2)} as const;`,
    '',
  ].join('\n');
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const periPath = resolvePeriBinary(options.periPath);
  const provider = await requestToolsList(periPath);
  const tools = provider.tools;
  if (!Array.isArray(tools) || tools.length === 0) throw new Error('provider returned no tools');
  const normalized = tools.map((tool) => normalizeTool(tool));
  const names = new Set(normalized.map((tool) => tool.name));
  if (names.size !== normalized.length) throw new Error('provider returned duplicate tool names');

  const providerVersion = provider.serverInfo?.version;
  if (typeof providerVersion !== 'string' || providerVersion.length === 0)
    throw new Error('provider returned no serverInfo.version');
  const snapshot = {
    providerName: typeof provider.serverInfo?.name === 'string' ? provider.serverInfo.name : 'peri',
    providerVersion,
    protocolVersion,
    sourceCommand: providerCommand,
    toolsDigest: digestOf(normalized),
    tools: normalized,
  };
  const content = renderSource(snapshot);

  if (options.check) {
    let current;
    try {
      current = readFileSync(outputPath, 'utf8');
    } catch {
      throw new Error(`${outputPath} is missing; run the generator without --check`);
    }
    if (current !== content)
      throw new Error(`${outputPath} is stale; rerun scripts/generate-workspace-tool-snapshot.mjs`);
    console.log(`workspace tool snapshot is current (${normalized.length} tools, provider ${providerVersion})`);
    return;
  }
  writeFileSync(outputPath, content);
  console.log(`wrote ${outputPath} (${normalized.length} tools, provider ${providerVersion})`);
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
