import { createReadStream } from 'node:fs';
import { mkdir, open, readFile, rename, lstat } from 'node:fs/promises';
import { createHash, randomUUID } from 'node:crypto';
import path from 'node:path';
import { canonicalJson, identityKey, ReplayError, sha256 } from './matcher.mjs';

const FORMAT_VERSION = 1;
const TERMINALS = new Set(['complete', 'client_abort', 'upstream_abort', 'upstream_error']);

async function writeDurable(file, content) {
  const handle = await open(file, 'wx', 0o600);
  try { await handle.writeFile(content); await handle.sync(); } finally { await handle.close(); }
}

async function hashFile(file) {
  const hash = createHash('sha256');
  let size = 0;
  for await (const chunk of createReadStream(file)) { hash.update(chunk); size += chunk.length; }
  return { sha256: hash.digest('hex'), size };
}

export async function createCassetteWriter(directory, { provenance = {}, maxResponseBytes = 64 * 1024 * 1024 } = {}) {
  const destination = path.resolve(directory);
  await mkdir(path.dirname(destination), { recursive: true });
  try { await lstat(destination); throw new ReplayError('cassette_exists', '目标 cassette 已存在，禁止覆盖'); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  const staging = `${destination}.partial-${randomUUID()}`;
  await mkdir(staging, { mode: 0o700 });
  await mkdir(path.join(staging, 'requests'), { mode: 0o700 });
  await mkdir(path.join(staging, 'responses'), { mode: 0o700 });
  const entries = new Map();
  const pending = new Set();
  const failures = [];
  let sealed = false;
  let sealing = false;

  return {
    directory: destination,
    staging,
    entries,
    async begin(identity, request, requestRules = []) {
      if (sealed || sealing) throw new ReplayError('cassette_sealed', 'cassette 已封存或正在封存');
      const key = identityKey(identity);
      if (entries.has(key) || pending.has(key)) throw new ReplayError('duplicate_identity', '录制身份重复', { identity });
      pending.add(key);
      const id = sha256(key);
      const requestFile = `requests/${id}.bin`;
      const responseFile = `responses/${id}.bin`;
      let handle;
      try {
        await writeDurable(path.join(staging, requestFile), request.body);
        handle = await open(path.join(staging, responseFile), 'wx', 0o600);
      } catch (error) { pending.delete(key); failures.push(String(error)); throw error; }
      const chunks = [];
      let offset = 0;
      let finished = false;
      const hash = createHash('sha256');
      const entry = {
        identity,
        requestRules,
        request: {
          method: request.method, target: request.target, headers: request.headers,
          canonicalBody: request.canonicalBody,
          file: requestFile, size: request.body.length, sha256: sha256(request.body),
        },
        response: { file: responseFile, status: null, headers: {}, chunks, size: 0, submittedBytes: 0, sha256: null, terminal: null },
      };
      return {
        setHeaders(status, headers, atMs) {
          entry.response.status = status;
          entry.response.headers = headers;
          entry.response.headersAtMs = atMs;
        },
        setProtocolFacts(facts) { entry.response.protocolFacts = facts; },
        setSubmittedBytes(bytes) { entry.response.submittedBytes = bytes; },
        async append(bytes, atMs) {
          if (finished) throw new ReplayError('entry_finished', '不能向终态录制追加内容');
          if (offset + bytes.length > maxResponseBytes) throw new ReplayError('recording_too_large', '响应超过录制上限，当前 cassette 不可封存');
          await handle.writeFile(bytes);
          chunks.push({ offset, length: bytes.length, atMs, sha256: sha256(bytes) });
          hash.update(bytes);
          offset += bytes.length;
        },
        async finish(kind, atMs, detail) {
          if (finished) throw new ReplayError('entry_finished', '录制重复终结');
          finished = true;
          try {
            await handle.sync();
            await handle.close();
            if (!TERMINALS.has(kind)) throw new ReplayError('invalid_terminal', '未知录制终态');
            entry.response.size = offset;
            entry.response.sha256 = hash.digest('hex');
            entry.response.terminal = { kind, atMs, ...(detail ? { detail } : {}) };
            entries.set(key, entry);
          } catch (error) { failures.push(String(error)); throw error; }
          finally { pending.delete(key); }
          return entry;
        },
        async invalidate(error) {
          if (finished) return;
          finished = true;
          pending.delete(key);
          failures.push(String(error));
          await handle.close();
        },
      };
    },
    async seal() {
      if (sealed || sealing) throw new ReplayError('cassette_sealed', 'cassette 已封存或正在封存');
      if (pending.size || failures.length || !entries.size) throw new ReplayError('unsealable_cassette', '存在在途请求、录制错误或空录制', { pending: pending.size, failures });
      sealing = true;
      const manifest = {
        formatVersion: FORMAT_VERSION,
        kind: 'raw-http',
        provenance,
        entries: [...entries.values()].sort((a, b) => identityKey(a.identity).localeCompare(identityKey(b.identity))),
      };
      const bytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
      await writeDurable(path.join(staging, 'manifest.json'), bytes);
      await writeDurable(path.join(staging, 'seal.json'), `${JSON.stringify({ formatVersion: FORMAT_VERSION, manifestSha256: sha256(bytes) })}\n`);
      const dir = await open(staging, 'r');
      try { await dir.sync(); } finally { await dir.close(); }
      // 不覆盖已有目录；调用方只会把完整封存的目录作为可读 cassette。
      try { await lstat(destination); throw new ReplayError('cassette_exists', '封存目标已存在'); }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      await rename(staging, destination);
      const parent = await open(path.dirname(destination), 'r');
      try { await parent.sync(); } finally { await parent.close(); }
      sealed = true;
      return { directory: destination, entries: entries.size, manifestSha256: sha256(bytes) };
    },
  };
}

function validateEntry(entry) {
  const key = identityKey(entry.identity ?? {});
  const id = sha256(key);
  if (!entry.identity || !['owner', 'execution', 'call', 'purpose'].every((field) => typeof entry.identity[field] === 'string' && entry.identity[field])
      || !Number.isSafeInteger(entry.identity.attempt) || entry.identity.attempt < 1) {
    throw new ReplayError('corrupt_cassette', 'cassette 调用身份无效');
  }
  if (entry.request?.file !== `requests/${id}.bin` || entry.response?.file !== `responses/${id}.bin`) {
    throw new ReplayError('corrupt_cassette', 'cassette 文件路径不符合身份');
  }
  if (!TERMINALS.has(entry.response.terminal?.kind)) throw new ReplayError('corrupt_cassette', 'cassette 缺少合法终态');
  if (entry.response.status !== null && (!Number.isInteger(entry.response.status) || entry.response.status < 200 || entry.response.status > 599)) {
    throw new ReplayError('corrupt_cassette', 'cassette 响应状态无效');
  }
  let offset = 0;
  let atMs = 0;
  for (const chunk of entry.response.chunks ?? []) {
    if (chunk.offset !== offset || !Number.isSafeInteger(chunk.length) || chunk.length < 1 || !Number.isFinite(chunk.atMs) || chunk.atMs < atMs) {
      throw new ReplayError('corrupt_cassette', 'cassette 分块索引不连续');
    }
    offset += chunk.length;
    atMs = chunk.atMs;
  }
  if (offset !== entry.response.size) throw new ReplayError('corrupt_cassette', 'cassette 分块长度不符');
  if (!Number.isSafeInteger(entry.response.submittedBytes) || entry.response.submittedBytes < 0 || entry.response.submittedBytes > offset
      || (entry.response.terminal.kind === 'complete' && entry.response.submittedBytes !== offset)) {
    throw new ReplayError('corrupt_cassette', 'cassette 发送前缀与观察字节不一致');
  }
  if (entry.response.status === null && (offset || !['upstream_error', 'client_abort'].includes(entry.response.terminal.kind))) {
    throw new ReplayError('corrupt_cassette', '无响应头的录制必须为连接失败');
  }
  return key;
}

export async function loadCassette(directory) {
  const root = path.resolve(directory);
  let manifestBytes, seal;
  try {
    manifestBytes = await readFile(path.join(root, 'manifest.json'));
    seal = JSON.parse(await readFile(path.join(root, 'seal.json'), 'utf8'));
  } catch (error) { throw new ReplayError('unsealed_cassette', 'cassette 未封存或无法读取', { reason: String(error) }); }
  if (seal.formatVersion !== FORMAT_VERSION || seal.manifestSha256 !== sha256(manifestBytes)) {
    throw new ReplayError('corrupt_cassette', 'cassette manifest 校验失败');
  }
  const manifest = JSON.parse(manifestBytes.toString('utf8'));
  if (manifest.formatVersion !== FORMAT_VERSION || manifest.kind !== 'raw-http' || !Array.isArray(manifest.entries) || !manifest.entries.length) {
    throw new ReplayError('corrupt_cassette', '不支持或为空的 cassette');
  }
  const entries = new Map();
  for (const entry of manifest.entries) {
    const key = validateEntry(entry);
    if (entries.has(key)) throw new ReplayError('corrupt_cassette', 'cassette 身份重复');
    for (const body of [entry.request, entry.response]) {
      const file = path.join(root, body.file);
      const stat = await lstat(file);
      if (!stat.isFile() || stat.isSymbolicLink()) throw new ReplayError('corrupt_cassette', 'cassette body 必须为普通文件');
      const actual = await hashFile(file);
      if (actual.size !== body.size || actual.sha256 !== body.sha256) throw new ReplayError('corrupt_cassette', 'cassette body 校验失败', { file: body.file });
    }
    if (typeof entry.request.canonicalBody !== 'string' || canonicalJson(entry.request.headers) === undefined) {
      throw new ReplayError('corrupt_cassette', 'cassette 请求匹配信息缺失');
    }
    entries.set(key, entry);
  }
  return { directory: root, manifest, entries };
}

export async function* readResponseChunks(cassette, entry) {
  const handle = await open(path.join(cassette.directory, entry.response.file), 'r');
  try {
    for (const part of entry.response.chunks) {
      const bytes = Buffer.alloc(part.length);
      let read = 0;
      while (read < bytes.length) {
        const result = await handle.read(bytes, read, bytes.length - read, part.offset + read);
        if (!result.bytesRead) throw new ReplayError('corrupt_cassette', '重放 body 提前结束');
        read += result.bytesRead;
      }
      if (sha256(bytes) !== part.sha256) throw new ReplayError('corrupt_cassette', '重放分块校验失败');
      yield { bytes, atMs: part.atMs };
    }
  } finally { await handle.close(); }
}
