#!/usr/bin/env node
import http from 'node:http';
import { randomBytes } from 'node:crypto';
import { parseArgs } from 'node:util';
import { readFile } from 'node:fs/promises';
import { startReplayProxy } from './proxy.mjs';
import { loadCassette } from './cassette.mjs';

const usage = `用法：
  node e2e/llm-replay/cli.mjs record --cassette DIR --run RUN [--port 0]
  node e2e/llm-replay/cli.mjs replay --cassette DIR --run RUN [--timing fast|recorded]
  node e2e/llm-replay/cli.mjs verify --cassette DIR

record 可重复给 --upstream LOGICAL_ORIGIN=TRANSPORT_ORIGIN，用本地 fixture 替换真实上游。
--gates FILE 读取 [{identity:{owner,execution,call,attempt,purpose},afterBytes:N}] 控制门。
代理与管理端口均只监听 127.0.0.1；启动输出包含管理端口和一次性控制 token。
所有请求必须带 x-peri-replay-context 和 x-peri-replay-target。
Ctrl+C 仅在 run 已满足严格完成条件时封存；中途结束保留不可重放的 partial 目录。
`;

try {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      cassette: { type: 'string' }, run: { type: 'string' }, port: { type: 'string', default: '0' },
      timing: { type: 'string', default: 'fast' }, upstream: { type: 'string', multiple: true, default: [] },
      gates: { type: 'string' },
      help: { type: 'boolean', short: 'h' },
    },
  });
  if (values.help) { console.log(usage); }
  else {
    const command = positionals[0];
    if (positionals.length !== 1 || !['record', 'replay', 'verify'].includes(command) || !values.cassette) throw new Error(usage);
    if (command === 'verify') {
      const cassette = await loadCassette(values.cassette);
      console.log(JSON.stringify({ passed: true, entries: cassette.entries.size, directory: cassette.directory }));
    } else {
      if (!values.run) throw new Error('--run 为必填参数');
      const port = Number(values.port);
      if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error('--port 必须为合法端口');
      const upstreams = values.upstream.map((mapping) => {
        const divider = mapping.indexOf('=');
        if (divider < 1) throw new Error('--upstream 格式为 LOGICAL_ORIGIN=TRANSPORT_ORIGIN');
        return { logicalOrigin: mapping.slice(0, divider), transportOrigin: mapping.slice(divider + 1) };
      });
      const gates = values.gates ? JSON.parse(await readFile(values.gates, 'utf8')) : [];
      const proxy = await startReplayProxy({ mode: command, cassetteDir: values.cassette, port, timing: values.timing, upstreams });
      let run;
      try { run = proxy.createRun(values.run, { gates }); }
      catch (error) { await proxy.close(); throw error; }
      const adminToken = randomBytes(24).toString('base64url');
      let sealed = false;
      const admin = http.createServer(async (request, response) => {
        try {
          if (request.headers['x-peri-replay-admin'] !== adminToken) {
            response.writeHead(403); response.end(); return;
          }
          let result;
          if (request.method === 'GET' && request.url === '/status') result = proxy.snapshot(values.run);
          else if (request.method === 'POST' && request.url === '/complete') result = proxy.completeRun(values.run);
          else if (request.method === 'POST' && request.url === '/seal') { result = await proxy.seal(); sealed = true; }
          else if (request.method === 'POST' && ['/bind', '/release'].includes(request.url)) {
            const chunks = []; let length = 0;
            for await (const chunk of request) {
              length += chunk.length;
              if (length > 65536) throw new Error('管理请求过大');
              chunks.push(chunk);
            }
            const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
            if (request.url === '/bind') run.bindIdentity(body);
            else run.release(body.identity, body.throughBytes ?? Infinity);
            result = { ok: true };
          } else { response.writeHead(404); response.end(); return; }
          response.writeHead(200, { 'content-type': 'application/json' });
          response.end(JSON.stringify(result));
        } catch (error) {
          response.writeHead(409, { 'content-type': 'application/json' });
          response.end(JSON.stringify({ error: error.code ?? 'admin_error', message: error.message, details: error.details ?? {} }));
        }
      });
      await new Promise((resolve) => admin.listen(0, '127.0.0.1', resolve));
      console.log(JSON.stringify({ mode: command, proxy: proxy.url, run: values.run, admin: `http://127.0.0.1:${admin.address().port}`, adminToken }));
      let stopping = false;
      const stop = async () => {
        if (stopping) return;
        stopping = true;
        try {
          const summary = proxy.completeRun(values.run);
          if (command === 'record' && !sealed) { await proxy.seal(); sealed = true; }
          console.log(JSON.stringify(summary));
        } catch (error) {
          console.error(JSON.stringify({ error: error.code ?? 'completion_error', message: error.message, details: error.details ?? {} }));
          process.exitCode = 1;
        } finally {
          admin.closeAllConnections();
          await new Promise((resolve) => admin.close(resolve));
          await proxy.close();
        }
      };
      process.once('SIGINT', stop);
      process.once('SIGTERM', stop);
    }
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
