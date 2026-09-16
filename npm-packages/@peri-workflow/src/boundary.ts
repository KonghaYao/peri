/**
 * Deterministic, bounded filesystem boundary evidence for ADLC.
 *
 * This is deliberately a CLI helper. Git remains the source of truth for
 * tracked/index/untracked facts; this module covers ignored files and records
 * the exact scope and coverage of the filesystem observation.
 */
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  opendirSync,
  openSync,
  readlinkSync,
  readSync,
  realpathSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import type { Stats } from 'node:fs'
import { dirname, isAbsolute, join, relative, sep } from 'node:path'
import { performance } from 'node:perf_hooks'

export const BOUNDARY_SCHEMA_VERSION = 1
export const BOUNDARY_SCANNER_VERSION = 'adlc-boundary-v1'
/** Fixed protocol limits: request/baseline parsing must be bounded before scan limits apply. */
export const MAX_BOUNDARY_REQUEST_BYTES = 1024 * 1024
export const MAX_BOUNDARY_BASELINE_BYTES = 64 * 1024 * 1024
export const MAX_BOUNDARY_ENTRIES = 100_000
export const MAX_BOUNDARY_BYTES = 1_073_741_824
export const MAX_BOUNDARY_DEPTH = 128
export const MAX_BOUNDARY_DEADLINE_MS = 60_000
const MAX_GIT_OUTPUT_BYTES = 1024 * 1024
const IO_CHUNK_BYTES = 1024 * 1024
const MAX_BOUNDARY_ROOT_DECLARATIONS = 1024

export type BoundaryRoot = string | { path: string; kind?: string }

export interface BoundaryLimits {
  maxEntries: number
  maxBytes: number
  maxDepth: number
  deadlineMs: number
}

export interface BoundaryRequest {
  schemaVersion: number
  operation: 'snapshot' | 'compare'
  baselineId: string
  repoRoot: string
  cwd?: string
  observedRoots?: BoundaryRoot[]
  generatedRoots?: BoundaryRoot[]
  skipRoots?: BoundaryRoot[]
  pathAllowlist?: string[]
  limits: BoundaryLimits
  baselinePath: string
  /** Main-agent-owned digest; never derive trust from the baseline file itself. */
  expectedBaselineFingerprint?: string
}

interface NormalizedRequest {
  schemaVersion: number
  baselineId: string
  repoRoot: string
  cwd: string
  observedRoots: string[]
  generatedRoots: string[]
  skipRoots: string[]
  pathAllowlist: string[]
  limits: BoundaryLimits
  baselinePath: string
  expectedBaselineFingerprint?: string
  requestFingerprint: string
}

interface Entry {
  path: string
  type: 'file' | 'directory' | 'symlink' | 'other'
  size: number
  digest?: string
  target?: string
  identity?: Identity
}

interface Identity {
  dev: number
  ino: number
  birthtimeMs: number
}

interface GeneratedRootState {
  path: string
  exists: boolean
  type: 'directory' | 'absent'
  identity?: Identity
}

interface Coverage {
  complete: boolean
  scope: 'full' | 'partial'
  readFiles: number
  readBytes: number
  elapsedMs: number
  maxEntries: number
  maxBytes: number
  maxDepth: number
  deadlineMs: number
  entries: number
  excludedRoots: string[]
  errors: string[]
}

interface SnapshotDocument {
  schemaVersion: number
  scannerVersion: string
  baselineId: string
  requestFingerprint: string
  repoRoot: string
  cwd: string
  baselinePath: string
  entries: Entry[]
  generatedRoots: GeneratedRootState[]
  coverage: Coverage
  fingerprint: string
}

export interface BoundarySummary {
  ok: boolean
  operation: 'snapshot' | 'compare'
  baselineId: string
  baselinePath: string
  scannerVersion: string
  readFiles: number
  readBytes: number
  elapsedMs: number
  coverage: Coverage
  fingerprint?: string
  changes?: {
    created: string[]
    removed: string[]
    typeChanged: string[]
    contentChanged: string[]
    allowed: string[]
    outOfScope: string[]
    generatedRootChanges: string[]
  }
}

class BoundaryError extends Error {}

function fail(message: string): never {
  throw new BoundaryError(message)
}

function positiveInteger(value: unknown, name: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) {
    fail(`${name} must be a positive safe integer`)
  }
  return value
}

function boundedInteger(value: unknown, name: string, maximum: number): number {
  const result = positiveInteger(value, name)
  if (result > maximum) fail(`${name} exceeds hard maximum ${maximum}`)
  return result
}

function rootPath(value: BoundaryRoot): string {
  const path = typeof value === 'string' ? value : value.path
  if (typeof path !== 'string' || path.length === 0) fail('boundary root path must be non-empty')
  return path
}

function validateRelative(value: string, field: string): string {
  if (isAbsolute(value)) fail(`${field} must be repository-relative`)
  const normalized = value.replaceAll('\\', '/')
  if (normalized === '' || normalized === '.') return '.'
  const parts = normalized.split('/')
  if (parts.some((part) => part === '..' || part === '')) fail(`${field} contains unsafe path components`)
  return parts.filter((part) => part !== '.').join('/')
}

function toPosix(value: string): string {
  return value.split(sep).join('/')
}

function isWithin(root: string, candidate: string): boolean {
  const child = relative(root, candidate)
  return child === '' || (!child.startsWith('..' + sep) && child !== '..' && !isAbsolute(child))
}

function identity(path: string): Identity {
  return identityFromStat(lstatSync(path))
}

function identityFromStat(meta: Stats): Identity {
  return {
    dev: Number(meta.dev ?? 0),
    ino: Number(meta.ino ?? 0),
    birthtimeMs: Math.trunc(meta.birthtimeMs || 0),
  }
}

interface FileVersion extends Identity {
  size: number
  mtimeMs: number
  ctimeMs: number
  isFile: boolean
  isDirectory: boolean
}

function version(meta: Stats): FileVersion {
  return {
    ...identityFromStat(meta),
    size: meta.size,
    mtimeMs: meta.mtimeMs,
    ctimeMs: meta.ctimeMs,
    isFile: meta.isFile(),
    isDirectory: meta.isDirectory(),
  }
}

function sameVersion(a: FileVersion, b: FileVersion): boolean {
  return sameIdentity(a, b) && a.size === b.size && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs &&
    a.isFile === b.isFile && a.isDirectory === b.isDirectory
}

function lstatIfPresent(path: string): Stats | undefined {
  try {
    return lstatSync(path)
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code
    if (code === 'ENOENT' || code === 'ENOTDIR') return undefined
    throw error
  }
}

function openReadOnlyNoFollow(path: string): number {
  return openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
}

function readBoundedUtf8(path: string, maximum: number, label: string): string {
  const before = lstatIfPresent(path)
  if (!before || before.isSymbolicLink() || !before.isFile()) fail(`${label} must be a regular file`)
  if (before.size > maximum) fail(`${label} exceeds ${maximum} bytes`)
  const beforeVersion = version(before)
  const canonicalBefore = realpathSync(path)
  const fd = openReadOnlyNoFollow(path)
  const chunks: Buffer[] = []
  let total = 0
  try {
    const opened = fstatSync(fd)
    if (!opened.isFile() || !sameVersion(beforeVersion, version(opened))) fail(`${label} race detected`)
    const buffer = Buffer.allocUnsafe(Math.min(IO_CHUNK_BYTES, Math.max(1, before.size)))
    while (total < before.size) {
      const count = readSync(fd, buffer, 0, Math.min(buffer.length, before.size - total), total)
      if (count === 0) fail(`${label} changed while reading`)
      chunks.push(Buffer.from(buffer.subarray(0, count)))
      total += count
      if (total > maximum) fail(`${label} exceeds ${maximum} bytes`)
    }
    const after = fstatSync(fd)
    const canonicalAfter = realpathSync(path)
    if (!after.isFile() || total !== after.size || !sameVersion(beforeVersion, version(after)) || canonicalAfter !== canonicalBefore) {
      fail(`${label} race detected`)
    }
  } finally {
    closeSync(fd)
  }
  return Buffer.concat(chunks, total).toString('utf8')
}

function sameIdentity(a?: Identity, b?: Identity): boolean {
  return !!a && !!b && a.dev === b.dev && a.ino === b.ino && a.birthtimeMs === b.birthtimeMs
}

function canonicalRepoRoot(raw: string): string {
  if (typeof raw !== 'string' || raw.length === 0 || !isAbsolute(raw)) {
    fail('repoRoot must be an existing absolute path')
  }
  const root = realpathSync(raw)
  if (!statSync(root).isDirectory()) fail('repoRoot must be a directory')
  return root
}

function resolveExistingOrParent(repoRoot: string, raw: string, field: string): { path: string; relative: string; exists: boolean } {
  const rel = validateRelative(raw, field)
  const candidate = join(repoRoot, rel)
  const existing = lstatIfPresent(candidate)
  const parent = existing ? candidate : dirname(candidate)
  const canonicalParent = realpathSync(parent)
  if (!isWithin(repoRoot, canonicalParent)) {
    fail(`${field} escapes repoRoot through a symlink`)
  }
  if (!existing) return { path: candidate, relative: rel, exists: false }
  const canonical = realpathSync(candidate)
  if (!isWithin(repoRoot, canonical)) {
    fail(`${field} escapes repoRoot through a symlink`)
  }
  return { path: canonical, relative: rel, exists: true }
}

function normalizeRequest(request: BoundaryRequest): NormalizedRequest {
  if (!request || request.schemaVersion !== BOUNDARY_SCHEMA_VERSION) fail('unsupported boundary schemaVersion')
  if (request.operation !== 'snapshot' && request.operation !== 'compare') fail('operation must be snapshot or compare')
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(request.baselineId)) {
    fail('baselineId must be a lowercase UUID')
  }
  if (request.expectedBaselineFingerprint !== undefined && !/^sha256:[0-9a-f]{64}$/.test(request.expectedBaselineFingerprint)) fail('expectedBaselineFingerprint must be sha256:<64 lowercase hex>')
  const repoRoot = canonicalRepoRoot(request.repoRoot)
  const cwd = request.cwd ? realpathSync(request.cwd) : repoRoot
  if (!isWithin(repoRoot, cwd)) fail('cwd escapes repoRoot')
  const limits = request.limits
  if (!limits) fail('limits are required')
  const normalizedLimits = {
    maxEntries: boundedInteger(limits.maxEntries, 'limits.maxEntries', MAX_BOUNDARY_ENTRIES),
    maxBytes: boundedInteger(limits.maxBytes, 'limits.maxBytes', MAX_BOUNDARY_BYTES),
    maxDepth: boundedInteger(limits.maxDepth, 'limits.maxDepth', MAX_BOUNDARY_DEPTH),
    deadlineMs: boundedInteger(limits.deadlineMs, 'limits.deadlineMs', MAX_BOUNDARY_DEADLINE_MS),
  }
  for (const [field, value] of [
    ['observedRoots', request.observedRoots],
    ['generatedRoots', request.generatedRoots],
    ['skipRoots', request.skipRoots],
    ['pathAllowlist', request.pathAllowlist],
  ] as const) {
    if (value !== undefined && (!Array.isArray(value) || value.length > MAX_BOUNDARY_ROOT_DECLARATIONS)) {
      fail(`${field} exceeds hard maximum ${MAX_BOUNDARY_ROOT_DECLARATIONS}`)
    }
  }
  const roots = (request.observedRoots ?? ['.']).map((root) => resolveExistingOrParent(repoRoot, rootPath(root), 'observedRoots').relative)
  const generatedRoots = (request.generatedRoots ?? []).map((root) => resolveExistingOrParent(repoRoot, rootPath(root), 'generatedRoots').relative)
  if (generatedRoots.some((root) => root === '.')) fail('generatedRoots cannot authorize repoRoot')
  const skipRoots = (request.skipRoots ?? ['.git']).map((root) => resolveExistingOrParent(repoRoot, rootPath(root), 'skipRoots').relative)
  const pathAllowlist = (request.pathAllowlist ?? []).map((path) => validateRelative(path, 'pathAllowlist'))
  for (const generated of generatedRoots) {
    if (!isAllowed(generated, pathAllowlist)) fail(`generated root is outside pathAllowlist: ${generated}`)
  }
  const baseline = resolveExistingOrParent(repoRoot, request.baselinePath, 'baselinePath')
  if (baseline.exists && request.operation === 'snapshot') fail('baselinePath already exists')
  if (baseline.exists && lstatSync(baseline.path).isSymbolicLink()) fail('baselinePath must not be a symlink')
  if (!isWithin(repoRoot, baseline.path) || baseline.relative === '.') fail('baselinePath must be below repoRoot')
  const identityInput = {
    schemaVersion: request.schemaVersion,
    baselineId: request.baselineId,
    repoRoot,
    cwd,
    observedRoots: [...new Set(roots)].sort(),
    generatedRoots: [...new Set(generatedRoots)].sort(),
    skipRoots: [...new Set(skipRoots)].sort(),
    pathAllowlist: [...new Set(pathAllowlist)].sort(),
    limits: normalizedLimits,
    baselinePath: baseline.relative,
    scannerVersion: BOUNDARY_SCANNER_VERSION,
  }
  const requestFingerprint = sha256(stableJson(identityInput))
  return { ...identityInput, requestFingerprint, expectedBaselineFingerprint: request.expectedBaselineFingerprint }
}

function stableJson(value: unknown): string {
  return JSON.stringify(value)
}

function sha256(value: string | Uint8Array): string {
  return createHash('sha256').update(value).digest('hex')
}

function isDescendant(path: string, root: string): boolean {
  return path === root || path.startsWith(`${root}/`)
}

function isAllowed(path: string, allowlist: string[]): boolean {
  return allowlist.some((allowed) => allowed === '.' || path === allowed || path.startsWith(`${allowed}/`))
}

function targetOf(path: string): string {
  return readlinkSync(path, 'utf8')
}

function generatedRootHasTrackedFiles(repoRoot: string, path: string, deadlineAt: number): boolean {
  const remainingMs = Math.floor(deadlineAt - performance.now())
  if (remainingMs <= 0) fail('boundary scan deadline exceeded')
  const result = spawnSync('git', ['-C', repoRoot, 'ls-files', '-z', '--', `:(literal)${path}`], {
    encoding: 'buffer',
    maxBuffer: MAX_GIT_OUTPUT_BYTES,
    timeout: remainingMs,
  })
  if (result.error || result.status !== 0 || result.stdout.length >= MAX_GIT_OUTPUT_BYTES) {
    fail(`cannot establish tracked-file status for generated root: ${path}`)
  }
  return result.stdout.length > 0
}

function generatedState(repoRoot: string, paths: string[], deadlineAt: number): GeneratedRootState[] {
  return paths.map((path) => {
    const absolute = join(repoRoot, path)
    const stat = lstatIfPresent(absolute)
    if (generatedRootHasTrackedFiles(repoRoot, path, deadlineAt)) fail(`generated root contains tracked files: ${path}`)
    if (!stat) return { path, exists: false, type: 'absent' }
    if (!stat.isDirectory() || stat.isSymbolicLink()) fail(`generated root must be a real directory: ${path}`)
    return { path, exists: true, type: 'directory', identity: identity(absolute) }
  })
}

interface GuardedDirectory {
  canonical: string
  version: FileVersion
}

function guardDirectory(repoRoot: string, absolute: string, path: string): GuardedDirectory {
  const before = lstatIfPresent(absolute)
  if (!before || before.isSymbolicLink() || !before.isDirectory()) fail(`directory changed while scanning: ${path}`)
  const canonical = realpathSync(absolute)
  if (!isWithin(repoRoot, canonical)) fail(`directory escapes repoRoot while scanning: ${path}`)
  const fd = openReadOnlyNoFollow(absolute)
  let opened: ReturnType<typeof fstatSync>
  try {
    opened = fstatSync(fd)
  } finally {
    closeSync(fd)
  }
  if (!opened.isDirectory() || !sameVersion(version(before), version(opened))) fail(`directory race detected: ${path}`)
  return { canonical, version: version(before) }
}

function verifyDirectory(repoRoot: string, absolute: string, path: string, expected: GuardedDirectory): void {
  const after = lstatIfPresent(absolute)
  if (!after || after.isSymbolicLink() || !after.isDirectory()) fail(`directory race detected: ${path}`)
  const canonical = realpathSync(absolute)
  if (canonical !== expected.canonical || !isWithin(repoRoot, canonical) || !sameVersion(expected.version, version(after))) {
    fail(`directory race detected: ${path}`)
  }
}

function hashRegularFile(
  repoRoot: string,
  absolute: string,
  path: string,
  remainingBytes: number,
  checkBudget: () => void,
): { size: number; digest: string } {
  const before = lstatIfPresent(absolute)
  if (!before || before.isSymbolicLink() || !before.isFile()) fail(`file changed while scanning: ${path}`)
  const beforeVersion = version(before)
  if (before.size > remainingBytes) fail('boundary scan maxBytes exceeded')
  const canonicalBefore = realpathSync(absolute)
  if (!isWithin(repoRoot, canonicalBefore)) fail(`file escapes repoRoot while scanning: ${path}`)
  const fd = openReadOnlyNoFollow(absolute)
  const hash = createHash('sha256')
  const buffer = Buffer.allocUnsafe(Math.min(IO_CHUNK_BYTES, Math.max(1, before.size)))
  let position = 0
  try {
    const opened = fstatSync(fd)
    if (!opened.isFile() || !sameVersion(beforeVersion, version(opened))) fail(`file race detected: ${path}`)
    while (position < before.size) {
      checkBudget()
      const count = readSync(fd, buffer, 0, Math.min(buffer.length, before.size - position), position)
      if (count === 0) fail(`boundary file changed while reading: ${path}`)
      hash.update(buffer.subarray(0, count))
      position += count
    }
    const after = fstatSync(fd)
    const canonicalAfter = realpathSync(absolute)
    if (!after.isFile() || !sameVersion(beforeVersion, version(after)) || canonicalAfter !== canonicalBefore || !isWithin(repoRoot, canonicalAfter)) {
      fail(`file race detected: ${path}`)
    }
  } finally {
    closeSync(fd)
  }
  return { size: before.size, digest: `sha256:${hash.digest('hex')}` }
}

function scan(request: NormalizedRequest): { entries: Entry[]; generatedRoots: GeneratedRootState[]; coverage: Coverage } {
  const started = performance.now()
  let readFiles = 0
  let readBytes = 0
  let entriesCount = 0
  let enumeratedEntries = 0
  const entries: Entry[] = []
  const generatedRoots = generatedState(request.repoRoot, request.generatedRoots, started + request.limits.deadlineMs)
  const excludedRoots = [...new Set([...request.skipRoots, ...request.generatedRoots])].sort()
  const generatedSet = new Set(request.generatedRoots)
  const skipSet = new Set(request.skipRoots)
  const rootEntries = request.observedRoots
  const fullScope = rootEntries.length === 1 && rootEntries[0] === '.'
  const errors: string[] = []
  const checkBudget = () => {
    if (performance.now() - started > request.limits.deadlineMs) fail('boundary scan deadline exceeded')
    if (entriesCount >= request.limits.maxEntries) fail('boundary scan maxEntries exceeded')
  }
  const childrenOf = (absolute: string): string[] => {
    const dir = opendirSync(absolute)
    const children: string[] = []
    try {
      for (;;) {
        if (performance.now() - started > request.limits.deadlineMs) fail('boundary scan deadline exceeded')
        const entry = dir.readSync()
        if (!entry) break
        enumeratedEntries += 1
        if (enumeratedEntries > request.limits.maxEntries) fail('boundary scan maxEntries exceeded')
        children.push(entry.name)
      }
    } finally {
      dir.closeSync()
    }
    return children.sort()
  }
  const visit = (absolute: string, path: string, depth: number) => {
    checkBudget()
    if (path === request.baselinePath) return
    if (generatedSet.has(path)) return
    if (skipSet.has(path)) return
    if (depth > request.limits.maxDepth) fail('boundary scan maxDepth exceeded')
    let stat
    try {
      stat = lstatSync(absolute)
    } catch (error) {
      fail(`boundary entry cannot be read: ${path}: ${(error as Error).message}`)
    }
    entriesCount += 1
    if (stat.isSymbolicLink()) {
      entries.push({ path, type: 'symlink', size: stat.size, target: targetOf(absolute) })
      return
    }
    if (stat.isDirectory()) {
      const guarded = guardDirectory(request.repoRoot, absolute, path)
      entries.push({ path, type: 'directory', size: 0, identity: identityFromStat(stat) })
      const children = childrenOf(absolute)
      for (const child of children) visit(join(absolute, child), `${path}/${child}`, depth + 1)
      verifyDirectory(request.repoRoot, absolute, path, guarded)
      return
    }
    if (stat.isFile()) {
      const file = hashRegularFile(request.repoRoot, absolute, path, request.limits.maxBytes - readBytes, checkBudget)
      readFiles += 1
      readBytes += file.size
      entries.push({ path, type: 'file', size: file.size, digest: file.digest })
      return
    }
    entries.push({ path, type: 'other', size: stat.size })
  }
  for (const root of rootEntries) {
    if (root === '.') {
      const guarded = guardDirectory(request.repoRoot, request.repoRoot, '.')
      const children = childrenOf(request.repoRoot)
      for (const child of children) visit(join(request.repoRoot, child), child, 1)
      verifyDirectory(request.repoRoot, request.repoRoot, '.', guarded)
    } else if (generatedSet.has(root) || skipSet.has(root)) {
      continue
    } else {
      const absolute = join(request.repoRoot, root)
      if (!lstatIfPresent(absolute)) continue
      visit(absolute, root, 0)
    }
  }
  // Avoid locale-dependent ordering in the serialized fingerprint.
  entries.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
  const elapsedMs = Math.max(0, Math.round(performance.now() - started))
  return {
    entries,
    generatedRoots,
    coverage: {
      complete: fullScope && request.skipRoots.every((root) => root === '.git') && errors.length === 0,
      scope: fullScope ? 'full' : 'partial',
      readFiles,
      readBytes,
      elapsedMs,
      maxEntries: request.limits.maxEntries,
      maxBytes: request.limits.maxBytes,
      maxDepth: request.limits.maxDepth,
      deadlineMs: request.limits.deadlineMs,
      entries: entries.length,
      excludedRoots,
      errors,
    },
  }
}

function snapshotDocument(request: NormalizedRequest, scanResult: ReturnType<typeof scan>): SnapshotDocument {
  const body = {
    schemaVersion: BOUNDARY_SCHEMA_VERSION,
    scannerVersion: BOUNDARY_SCANNER_VERSION,
    baselineId: request.baselineId,
    requestFingerprint: request.requestFingerprint,
    repoRoot: request.repoRoot,
    cwd: request.cwd,
    baselinePath: request.baselinePath,
    entries: scanResult.entries,
    generatedRoots: scanResult.generatedRoots,
    coverage: scanResult.coverage,
  }
  return { ...body, fingerprint: `sha256:${sha256(stableJson(body))}` }
}

function writeExclusive(path: string, content: string): void {
  if (Buffer.byteLength(content, 'utf8') > MAX_BOUNDARY_BASELINE_BYTES) {
    fail(`boundary baseline exceeds ${MAX_BOUNDARY_BASELINE_BYTES} bytes`)
  }
  try {
    writeFileSync(path, content, { encoding: 'utf8', flag: 'wx' })
  } catch (error) {
    fail(`cannot publish boundary baseline: ${(error as Error).message}`)
  }
}

function summaryFromCoverage(operation: 'snapshot' | 'compare', request: NormalizedRequest, coverage: Coverage): BoundarySummary {
  return {
    ok: coverage.complete,
    operation,
    baselineId: request.baselineId,
    baselinePath: request.baselinePath,
    scannerVersion: BOUNDARY_SCANNER_VERSION,
    readFiles: coverage.readFiles,
    readBytes: coverage.readBytes,
    elapsedMs: coverage.elapsedMs,
    coverage,
  }
}

export function snapshotBoundary(request: BoundaryRequest): BoundarySummary {
  const normalized = normalizeRequest({ ...request, operation: 'snapshot' })
  const result = scan(normalized)
  const document = snapshotDocument(normalized, result)
  writeExclusive(join(normalized.repoRoot, normalized.baselinePath), `${JSON.stringify(document, null, 2)}\n`)
  return {
    ...summaryFromCoverage('snapshot', normalized, result.coverage),
    ok: result.coverage.complete,
    fingerprint: document.fingerprint,
  }
}

function readBaseline(normalized: NormalizedRequest): SnapshotDocument {
  const path = join(normalized.repoRoot, normalized.baselinePath)
  const meta = lstatIfPresent(path)
  if (!meta) fail('baselinePath does not exist')
  if (meta.isSymbolicLink() || !meta.isFile()) fail('baselinePath must be a regular file')
  if (meta.size > MAX_BOUNDARY_BASELINE_BYTES) fail(`boundary baseline exceeds ${MAX_BOUNDARY_BASELINE_BYTES} bytes`)
  let baseline: SnapshotDocument
  try {
    baseline = JSON.parse(readBoundedUtf8(path, MAX_BOUNDARY_BASELINE_BYTES, 'boundary baseline')) as SnapshotDocument
  } catch (error) {
    fail(`baseline JSON is invalid: ${(error as Error).message}`)
  }
  if (baseline.schemaVersion !== BOUNDARY_SCHEMA_VERSION || baseline.scannerVersion !== BOUNDARY_SCANNER_VERSION) fail('baseline schema or scanner version mismatch')
  if (baseline.baselineId !== normalized.baselineId) fail('baselineId does not match baseline file')
  if (normalized.expectedBaselineFingerprint && baseline.fingerprint !== normalized.expectedBaselineFingerprint) fail('baseline fingerprint does not match expectedBaselineFingerprint')
  if (baseline.repoRoot !== normalized.repoRoot || baseline.cwd !== normalized.cwd || baseline.requestFingerprint !== normalized.requestFingerprint) fail('baseline identity does not match request')
  if (!baseline.coverage?.complete) fail('baseline coverage is incomplete')
  const body = { ...baseline }
  delete (body as Partial<SnapshotDocument>).fingerprint
  if (baseline.fingerprint !== `sha256:${sha256(stableJson(body))}`) fail('baseline fingerprint mismatch')
  return baseline
}

export function compareBoundary(request: BoundaryRequest): BoundarySummary {
  const normalized = normalizeRequest({ ...request, operation: 'compare' })
  if (!normalized.expectedBaselineFingerprint) fail('compare requires expectedBaselineFingerprint')
  const baseline = readBaseline(normalized)
  const result = scan(normalized)
  const before = new Map(baseline.entries.map((entry) => [entry.path, entry]))
  const after = new Map(result.entries.map((entry) => [entry.path, entry]))
  const created: string[] = []
  const removed: string[] = []
  const typeChanged: string[] = []
  const contentChanged: string[] = []
  for (const path of [...new Set([...before.keys(), ...after.keys()])].sort()) {
    const a = before.get(path)
    const b = after.get(path)
    if (!a) created.push(path)
    else if (!b) removed.push(path)
    else if (a.type !== b.type) typeChanged.push(path)
    else if (a.type === 'file' && (a.size !== b.size || a.digest !== b.digest)) contentChanged.push(path)
    else if (a.type === 'symlink' && a.target !== b.target) contentChanged.push(path)
  }
  const changed = [...created, ...removed, ...typeChanged, ...contentChanged]
  const allowed = changed.filter((path) => isAllowed(path, normalized.pathAllowlist))
  const outOfScope = changed.filter((path) => !isAllowed(path, normalized.pathAllowlist))
  const generatedRootChanges: string[] = []
  for (const beforeRoot of baseline.generatedRoots) {
    const afterRoot = result.generatedRoots.find((root) => root.path === beforeRoot.path)
    if (!afterRoot) continue
    if ((beforeRoot.exists && !afterRoot.exists) || (beforeRoot.exists && afterRoot.exists && !sameIdentity(beforeRoot.identity, afterRoot.identity))) generatedRootChanges.push(beforeRoot.path)
  }
  const coverage = result.coverage
  const ok = coverage.complete && outOfScope.length === 0 && generatedRootChanges.length === 0
  return {
    ...summaryFromCoverage('compare', normalized, coverage),
    ok,
    fingerprint: baseline.fingerprint,
    changes: {
      created,
      removed,
      typeChanged,
      contentChanged,
      allowed,
      outOfScope,
      generatedRootChanges,
    },
  }
}

export function parseBoundaryRequest(path: string): BoundaryRequest {
  let value: unknown
  try {
    value = JSON.parse(readBoundedUtf8(path, MAX_BOUNDARY_REQUEST_BYTES, 'boundary request'))
  } catch (error) {
    fail(`boundary request is invalid: ${(error as Error).message}`)
  }
  if (!value || typeof value !== 'object') fail('boundary request must be a JSON object')
  return value as BoundaryRequest
}

export function boundaryCli(operation: 'snapshot' | 'compare', requestPath: string): BoundarySummary {
  const request = parseBoundaryRequest(requestPath)
  return operation === 'snapshot' ? snapshotBoundary(request) : compareBoundary(request)
}
