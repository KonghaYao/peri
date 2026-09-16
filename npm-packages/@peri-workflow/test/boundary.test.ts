import { describe, expect, test } from 'bun:test'
import { execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import {
  compareBoundary,
  snapshotBoundary,
  type BoundaryRequest,
} from '../src/boundary'

function git(cwd: string, ...args: string[]): void {
  execFileSync('git', args, { cwd, stdio: 'ignore' })
}

function fixture(): { repo: string; request: () => BoundaryRequest } {
  const repo = mkdtempSync(join(tmpdir(), 'workflow-boundary-'))
  mkdirSync(join(repo, '.peri', 'adlc', 'tasks', 'task', 'artifacts'), { recursive: true })
  mkdirSync(join(repo, '.claude', 'workflow-runs'), { recursive: true })
  mkdirSync(join(repo, 'allowed'), { recursive: true })
  mkdirSync(join(repo, 'ignored'), { recursive: true })
  mkdirSync(join(repo, 'tracked-generated'), { recursive: true })
  writeFileSync(join(repo, '.gitignore'), '.peri/\n.claude/\nignored/\n')
  writeFileSync(join(repo, 'tracked.txt'), 'base\n')
  writeFileSync(join(repo, 'allowed', 'change.txt'), 'base\n')
  writeFileSync(join(repo, 'ignored', 'existing.txt'), 'base\n')
  writeFileSync(join(repo, 'tracked-generated', 'state.txt'), 'tracked\n')
  symlinkSync('missing-target', join(repo, 'ignored', 'dangling'))
  git(repo, 'init', '-q')
  git(repo, 'config', 'user.name', 'Peri Test')
  git(repo, 'config', 'user.email', 'peri-test@example.invalid')
  git(repo, 'add', '.gitignore', 'tracked.txt', 'allowed', 'tracked-generated')
  git(repo, 'commit', '-qm', 'baseline')
  const id = randomUUID()
  const baselinePath = `.peri/adlc/tasks/task/artifacts/boundary-${id}.json`
  return {
    repo,
    request: () => ({
      schemaVersion: 1,
      operation: 'snapshot',
      baselineId: id,
      repoRoot: repo,
      cwd: repo,
      limits: { maxEntries: 1000, maxBytes: 10 * 1024 * 1024, maxDepth: 16, deadlineMs: 10_000 },
      baselinePath,
      pathAllowlist: ['allowed', '.peri/adlc/tasks/task', '.claude/workflow-runs/run-1'],
      generatedRoots: [{ path: '.claude/workflow-runs/run-1', kind: 'host-generated' }],
    }),
  }
}

function snapshotAndCompare(request: BoundaryRequest): { request: BoundaryRequest; baselineFingerprint: string } {
  const snapshot = snapshotBoundary(request)
  expect(snapshot.ok).toBe(true)
  expect(snapshot.readFiles).toBeGreaterThan(0)
  expect(snapshot.coverage.scope).toBe('full')
  const next = { ...request, operation: 'compare' as const, expectedBaselineFingerprint: snapshot.fingerprint }
  return { request: next, baselineFingerprint: snapshot.fingerprint as string }
}

describe('filesystem boundary snapshot/compare', () => {
  test('unchanged pre-existing dirty and ignored files pass', () => {
    const { repo, request } = fixture()
    writeFileSync(join(repo, 'tracked.txt'), 'dirty before workflow\n')
    writeFileSync(join(repo, 'ignored', 'existing.txt'), 'ignored dirty before workflow\n')
    const { request: compare } = snapshotAndCompare(request())
    const result = compareBoundary(compare)
    expect(result.ok).toBe(true)
    expect(result.changes?.outOfScope).toEqual([])
  })

  test('UUID v7 baselineId round-trips through snapshot and compare', () => {
    const { request } = fixture()
    const v7 = { ...request(), baselineId: '01923456-789a-7abc-8def-0123456789ab' }
    const snapshot = snapshotBoundary(v7)
    const result = compareBoundary({ ...v7, operation: 'compare', expectedBaselineFingerprint: snapshot.fingerprint })
    expect(result.ok).toBe(true)
    expect(result.baselineId).toBe(v7.baselineId)
  })

  test('ignored content change is reported out of scope', () => {
    const { repo, request } = fixture()
    const { request: compare } = snapshotAndCompare(request())
    writeFileSync(join(repo, 'ignored', 'existing.txt'), 'changed by external writer\n')
    const result = compareBoundary(compare)
    expect(result.ok).toBe(false)
    expect(result.changes?.contentChanged).toContain('ignored/existing.txt')
    expect(result.changes?.outOfScope).toContain('ignored/existing.txt')
  })

  test('ignored new external file is reported out of scope', () => {
    const { repo, request } = fixture()
    const { request: compare } = snapshotAndCompare(request())
    writeFileSync(join(repo, 'ignored', 'new.txt'), 'new external write\n')
    const result = compareBoundary(compare)
    expect(result.ok).toBe(false)
    expect(result.changes?.created).toContain('ignored/new.txt')
    expect(result.changes?.outOfScope).toContain('ignored/new.txt')
  })

  test('allowlisted content change passes and is attributed', () => {
    const { repo, request } = fixture()
    const { request: compare } = snapshotAndCompare(request())
    writeFileSync(join(repo, 'allowed', 'change.txt'), 'authorized change\n')
    const result = compareBoundary(compare)
    expect(result.ok).toBe(true)
    expect(result.changes?.allowed).toContain('allowed/change.txt')
    expect(result.changes?.outOfScope).toEqual([])
  })

  test('generated root allows internal content but rejects root replacement', () => {
    const { repo, request } = fixture()
    const { request: compare } = snapshotAndCompare(request())
    mkdirSync(join(repo, '.claude', 'workflow-runs', 'run-1'))
    writeFileSync(join(repo, '.claude', 'workflow-runs', 'run-1', 'state.json'), '{}')
    expect(compareBoundary(compare).ok).toBe(true)
    // A replacement with a symlink is never an authorized generated root.
    const root = join(repo, '.claude', 'workflow-runs', 'run-1')
    rmSync(root, { recursive: true, force: true })
    symlinkSync(repo, root)
    expect(() => compareBoundary(compare)).toThrow('generated root must be a real directory')
  })

  test('wrong expected fingerprint fails before scanning', () => {
    const { request } = fixture()
    const first = snapshotBoundary(request())
    expect(() => compareBoundary({ ...request(), operation: 'compare', expectedBaselineFingerprint: `sha256:${'0'.repeat(64)}` })).toThrow('expectedBaselineFingerprint')
    expect(first.fingerprint).toMatch(/^sha256:[0-9a-f]{64}$/)
    expect(() => compareBoundary({ ...request(), operation: 'compare' })).toThrow('requires expectedBaselineFingerprint')
  })

  test('generated root must not cover tracked subtree', () => {
    const { request } = fixture()
    expect(() => snapshotBoundary({
      ...request(),
      generatedRoots: [{ path: 'tracked-generated', kind: 'host-generated' }],
      pathAllowlist: [...(request().pathAllowlist ?? []), 'tracked-generated'],
    })).toThrow('contains tracked files')
  })

  test('generated root without Git provenance is unknown and cannot pass', () => {
    const { repo, request } = fixture()
    rmSync(join(repo, '.git'), { recursive: true, force: true })
    expect(() => snapshotBoundary(request())).toThrow('tracked-file status')
  })

  test('hard caps reject unbounded safe integers', () => {
    const { request } = fixture()
    expect(() => snapshotBoundary({ ...request(), limits: { ...request().limits, maxEntries: 100_001 } })).toThrow('hard maximum')
  })

  test('limit exhaustion is fail closed', () => {
    const { request } = fixture()
    expect(() => snapshotBoundary({ ...request(), limits: { maxEntries: 1, maxBytes: 10, maxDepth: 16, deadlineMs: 10_000 } })).toThrow('maxEntries')
  })

  test('baseline path is exclusive and path escape is rejected', () => {
    const { repo, request } = fixture()
    const first = snapshotBoundary(request())
    expect(existsSync(join(repo, request().baselinePath))).toBe(true)
    expect(readFileSync(join(repo, request().baselinePath), 'utf8')).toContain(first.baselineId)
    const baseline = JSON.parse(readFileSync(join(repo, request().baselinePath), 'utf8'))
    expect(baseline.entries.find((entry: { path: string }) => entry.path === 'ignored/dangling').target).toBe('missing-target')
    expect(() => snapshotBoundary(request())).toThrow('baselinePath already exists')
    expect(() => snapshotBoundary({ ...request(), baselinePath: '../outside.json' })).toThrow('unsafe path')
  })
})
