import { createReadStream } from 'node:fs';
import { mkdir, open, readFile, rename, lstat } from 'node:fs/promises';
import { createHash, randomUUID } from 'node:crypto';
import path from 'node:path';
import { canonicalBody, canonicalJson, identityKey, ReplayError, sha256 } from './matcher.mjs';
import {
  CASSETTE_FORMAT_VERSION, canonicalRecordedBody, compactTypedTemplateRules, decodeRequestStub,
  encodeRequestStub, expandTypedTemplateRules, loadWorld, serializeRequestStub,
} from './world-store.mjs';

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

export async function createCassetteWriter(directory, { provenance = {}, maxResponseBytes = 64 * 1024 * 1024, world } = {}) {
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
  const sealedFiles = new Map();
  let sealed = false;
  let sealing = false;

  return {
    directory: destination,
    staging,
    entries,
    async begin(identity, request, requestRules = [], { bindings, owner = 'world' } = {}) {
      if (sealed || sealing) throw new ReplayError('cassette_sealed', 'cassette 已封存或正在封存');
      const key = identityKey(identity);
      if (entries.has(key) || pending.has(key)) throw new ReplayError('duplicate_identity', '录制身份重复', { identity });
      pending.add(key);
      const id = sha256(key);
      const stubFile = `requests/${id}.stub.json`;
      const responseFile = `responses/${id}.bin`;
      let handle;
      let rules;
      try {
        rules = compactTypedTemplateRules(requestRules, { bindings, owner, bodyBytes: request.body });
        const canonical = request.canonicalBody ?? canonicalBody(request.body, { bindings, owner, rules: requestRules });
        const encoded = encodeRequestStub(request.body, { world });
        const stub = { ...encoded, canonicalSha256: sha256(canonical) };
        delete stub.legacy;
        delete stub.reason;
        if (encoded.legacy) {
          const legacyFile = `requests/${id}.legacy.bin`;
          await writeDurable(path.join(staging, legacyFile), request.body);
          sealedFiles.set(legacyFile, sha256(request.body));
          stub.fallback = encoded.reason;
          stub.parts = [{ raw: legacyFile }];
        }
        const stubBytes = serializeRequestStub(stub);
        await writeDurable(path.join(staging, stubFile), stubBytes);
        sealedFiles.set(stubFile, sha256(stubBytes));
        handle = await open(path.join(staging, responseFile), 'wx', 0o600);
      } catch (error) { pending.delete(key); failures.push(String(error)); throw error; }
      const chunks = [];
      let offset = 0;
      let finished = false;
      const hash = createHash('sha256');
      const entry = {
        identity,
        requestRules: rules,
        request: {
          method: request.method, target: request.target, headers: request.headers, stub: stubFile,
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
        formatVersion: CASSETTE_FORMAT_VERSION,
        kind: 'raw-http',
        provenance,
        entries: [...entries.values()].sort((a, b) => identityKey(a.identity).localeCompare(identityKey(b.identity))),
      };
      const bytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
      await writeDurable(path.join(staging, 'manifest.json'), bytes);
      const seal = {
        formatVersion: CASSETTE_FORMAT_VERSION,
        manifestSha256: sha256(bytes),
        files: Object.fromEntries([...sealedFiles.entries()].sort(([left], [right]) => left.localeCompare(right))),
      };
      await writeDurable(path.join(staging, 'seal.json'), `${JSON.stringify(seal)}\n`);
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

function validateEntry(entry, formatVersion = FORMAT_VERSION) {
  const key = identityKey(entry.identity ?? {});
  const id = sha256(key);
  if (!entry.identity || !['owner', 'execution', 'call', 'purpose'].every((field) => typeof entry.identity[field] === 'string' && entry.identity[field])
      || !Number.isSafeInteger(entry.identity.attempt) || entry.identity.attempt < 1) {
    throw new ReplayError('corrupt_cassette', 'cassette 调用身份无效');
  }
  const requestPath = formatVersion === CASSETTE_FORMAT_VERSION ? `requests/${id}.stub.json` : `requests/${id}.bin`;
  const actualRequest = formatVersion === CASSETTE_FORMAT_VERSION ? entry.request?.stub : entry.request?.file;
  if (actualRequest !== requestPath || entry.response?.file !== `responses/${id}.bin`) {
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

function cassetteFile(root, relative, label) {
  if (typeof relative !== 'string' || !relative || relative.includes('\0') || relative.includes('\\')
      || path.posix.isAbsolute(relative) || path.isAbsolute(relative)
      || path.posix.normalize(relative) !== relative || relative.startsWith('../')) {
    throw new ReplayError('corrupt_cassette', `cassette ${label} 路径无效`, { file: relative });
  }
  const resolved = path.resolve(root, relative);
  const rel = path.relative(root, resolved);
  if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) {
    throw new ReplayError('corrupt_cassette', `cassette ${label} 越出根目录`, { file: relative });
  }
  return resolved;
}

async function readSealedBody(root, relative, sealedFiles) {
  if (!Object.hasOwn(sealedFiles, relative)) {
    throw new ReplayError('corrupt_cassette', 'cassette 文件未纳入 seal', { file: relative });
  }
  const file = cassetteFile(root, relative, 'body');
  let stat;
  try { stat = await lstat(file); }
  catch (error) { throw new ReplayError('corrupt_cassette', 'cassette body 缺失', { file: relative, reason: String(error) }); }
  if (!stat.isFile() || stat.isSymbolicLink()) throw new ReplayError('corrupt_cassette', 'cassette body 必须为普通文件', { file: relative });
  const bytes = await readFile(file);
  if (sealedFiles[relative] !== sha256(bytes)) throw new ReplayError('corrupt_cassette', 'cassette body 校验失败', { file: relative });
  return bytes;
}

async function verifyResponseBody(root, entry) {
  const body = entry.response;
  const file = cassetteFile(root, body.file, 'response');
  let stat;
  try { stat = await lstat(file); }
  catch (error) { throw new ReplayError('corrupt_cassette', 'cassette body 缺失', { file: body.file, reason: String(error) }); }
  if (!stat.isFile() || stat.isSymbolicLink()) throw new ReplayError('corrupt_cassette', 'cassette body 必须为普通文件', { file: body.file });
  const actual = await hashFile(file);
  if (actual.size !== body.size || actual.sha256 !== body.sha256) throw new ReplayError('corrupt_cassette', 'cassette body 校验失败', { file: body.file });
}

async function loadCassetteV2(root, manifestBytes, seal, { world: suppliedWorld, worldDir } = {}) {
  if (!seal.files || typeof seal.files !== 'object' || Array.isArray(seal.files)) {
    throw new ReplayError('corrupt_cassette', 'cassette seal 缺少文件清单');
  }
  let manifest;
  try { manifest = JSON.parse(manifestBytes.toString('utf8')); }
  catch { throw new ReplayError('corrupt_cassette', 'cassette manifest 不是合法 JSON'); }
  if (manifest.formatVersion !== CASSETTE_FORMAT_VERSION || manifest.kind !== 'raw-http'
      || !Array.isArray(manifest.entries) || !manifest.entries.length) {
    throw new ReplayError('corrupt_cassette', '不支持或为空的 cassette');
  }
  const entries = new Map();
  const legacy = [];
  const worldDrift = [];
  let worldPromise;
  const resolveWorld = () => {
    if (!worldPromise) {
      worldPromise = (async () => {
        if (suppliedWorld) return suppliedWorld;
        const directory = worldDir ?? path.resolve(root, '..', '..', 'world');
        try { return await loadWorld(directory); }
        catch (error) {
          if (error instanceof ReplayError && error.code === 'unsealed_world') {
            throw new ReplayError('missing_world', 'cassette 引用了 world，但共享 world 不存在', { directory });
          }
          throw error;
        }
      })();
    }
    return worldPromise;
  };
  const readRaw = (relative) => readSealedBody(root, relative, seal.files);
  for (const entry of manifest.entries) {
    const key = validateEntry(entry, CASSETTE_FORMAT_VERSION);
    if (entries.has(key)) throw new ReplayError('corrupt_cassette', 'cassette 身份重复');
    const id = sha256(key);
    let stubBytes;
    try { stubBytes = await readSealedBody(root, entry.request.stub, seal.files); }
    catch (error) {
      if (error instanceof ReplayError) throw new ReplayError(error.code, error.message, { ...error.details, entry: id });
      throw error;
    }
    let stub;
    try { stub = JSON.parse(stubBytes.toString('utf8')); }
    catch { throw new ReplayError('invalid_request_stub', 'request stub 不是合法 JSON', { entry: id }); }
    const needsWorld = Array.isArray(stub?.parts) && stub.parts.some((part) => part && typeof part === 'object'
      && !Array.isArray(part) && (part.leaf !== undefined || part.recipe !== undefined));
    let decoded;
    try {
      decoded = await decodeRequestStub(stub, { world: needsWorld ? await resolveWorld() : undefined, readRaw, allowDrift: true });
    } catch (error) {
      if (error instanceof ReplayError) throw new ReplayError(error.code, error.message, { ...error.details, entry: id });
      throw error;
    }
    if (decoded.legacy) legacy.push({ entry: id, reason: stub.fallback ?? 'legacy_body' });
    if (decoded.drifted) worldDrift.push({ entry: id });
    let expanded;
    try { expanded = expandTypedTemplateRules(entry.requestRules ?? [], decoded.bytes); }
    catch (error) {
      if (error instanceof ReplayError) throw new ReplayError(error.code, error.message, { ...error.details, entry: id });
      throw error;
    }
    const canonical = (() => {
      try { return canonicalRecordedBody(decoded.bytes, expanded.rules, expanded.bindings); }
      catch (error) {
        if (error instanceof ReplayError) throw new ReplayError('corrupt_cassette', 'cassette canonical body 重建失败', { entry: id, reason: error.code });
        throw error;
      }
    })();
    // 世界漂移时 stub 中的 canonicalSha256 是录制值；以当前 world 重建的 canonical 作为期望。
    if (!decoded.drifted && (typeof stub.canonicalSha256 !== 'string' || sha256(canonical) !== stub.canonicalSha256)) {
      throw new ReplayError('corrupt_cassette', 'cassette canonical body 校验失败', { entry: id });
    }
    try { await verifyResponseBody(root, entry); }
    catch (error) {
      if (error instanceof ReplayError) throw new ReplayError(error.code, error.message, { ...error.details, entry: id });
      throw error;
    }
    entry.request = {
      method: entry.request.method, target: entry.request.target, headers: entry.request.headers,
      canonicalBody: canonical, stub: entry.request.stub,
    };
    entry.requestRules = expanded.rules;
    if (canonicalJson(entry.request.headers) === undefined) throw new ReplayError('corrupt_cassette', 'cassette 请求匹配信息缺失');
    entries.set(key, entry);
  }
  return { directory: root, manifest, entries, report: { formatVersion: CASSETTE_FORMAT_VERSION, legacy, worldDrift } };
}

export async function loadCassette(directory, options = {}) {
  const root = path.resolve(directory);
  let manifestBytes, seal;
  try {
    manifestBytes = await readFile(path.join(root, 'manifest.json'));
    seal = JSON.parse(await readFile(path.join(root, 'seal.json'), 'utf8'));
  } catch (error) { throw new ReplayError('unsealed_cassette', 'cassette 未封存或无法读取', { reason: String(error) }); }
  if (seal.formatVersion === CASSETTE_FORMAT_VERSION) {
    if (seal.manifestSha256 !== sha256(manifestBytes)) throw new ReplayError('corrupt_cassette', 'cassette manifest 校验失败');
    return loadCassetteV2(root, manifestBytes, seal, options);
  }
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
  return { directory: root, manifest, entries, report: { formatVersion: FORMAT_VERSION, legacy: [], worldDrift: [] } };
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
