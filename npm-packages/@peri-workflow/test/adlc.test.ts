import { afterEach, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { mkdtempSync, rmSync, symlinkSync, truncateSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { checkAdlcStage, MAX_ADLC_TOTAL_ARTIFACT_BYTES, planAdlcRecovery, type AdlcFingerprintSnapshot, type AdlcPlanRequest, type AdlcStageRequest } from '../src/adlc'
import { isCliCommand } from '../src/cli'

const roots: string[] = []

afterEach(() => {
  for (const root of roots) rmSync(root, { recursive: true, force: true })
  roots.length = 0
})

function fixture(content = 'verified\n'): { root: string; hash: string } {
  const root = mkdtempSync(join(tmpdir(), 'peri-adlc-'))
  roots.push(root)
  writeFileSync(join(root, 'handoff.md'), content)
  return { root, hash: `sha256:${createHash('sha256').update(content).digest('hex')}` }
}

function stage(root: string, output: AdlcStageRequest['requiredOutputs'], extra: Partial<AdlcStageRequest> = {}): AdlcStageRequest {
  return {
    schemaVersion: 1,
    adlcDeclared: true,
    stage: 'W2/Verify',
    canonicalRoot: root,
    currentRunId: 'run-current',
    requiredOutputs: output,
    ...extra,
  }
}

function fingerprints(tag: string): AdlcFingerprintSnapshot {
  const digest = createHash('sha256').update(tag).digest('hex')
  return {
    contract: `contract-${tag}`,
    input: `input-${tag}`,
    dependencies: { source: `dep-${tag}` },
    verifiedArtifacts: [{ path: 'handoff.md', sha256: `sha256:${digest}` }],
  }
}

describe('ADLC stage gate', () => {
  test('adlc is exposed as a CLI command', () => {
    expect(isCliCommand('adlc')).toBe(true)
  })

  test('valid non-empty artifact is ready and generic empty output remains allowed', () => {
    const { root, hash } = fixture()
    const result = checkAdlcStage(stage(root, [{ path: 'handoff.md', sha256: hash }]))
    expect(result.ready).toBe(true)
    expect(result.reusable).toEqual(['handoff.md'])
    expect(result.semanticAcceptance).toBe('not_checked')

    const generic = checkAdlcStage({ ...stage(root, []), adlcDeclared: false })
    expect(generic.ready).toBe(true)
    expect(generic.blockers).toEqual([])
  })

  test('ADLC empty, missing, empty, stale and symlink outputs are protocol blockers', () => {
    const { root, hash } = fixture('')
    symlinkSync(join(root, 'handoff.md'), join(root, 'handoff-link.md'))
    const result = checkAdlcStage(stage(root, []))
    expect(result.ready).toBe(false)
    expect(result.blockers[0]?.code).toBe('required_outputs_empty')

    const invalid = checkAdlcStage(stage(root, [
      { path: 'handoff.md', sha256: hash },
      { path: 'missing.md', sha256: hash },
      { path: 'handoff-link.md', sha256: hash },
    ]))
    expect(invalid.ready).toBe(false)
    expect(invalid.blockers.map((item) => item.code)).toEqual(expect.arrayContaining(['required_output_invalid']))

    const staleFixture = fixture('current\n')
    const stale = checkAdlcStage(stage(staleFixture.root, [{ path: 'handoff.md', sha256: 'sha256:' + 'a'.repeat(64) }]))
    expect(stale.blockers[0]?.code).toBe('required_output_stale')
  })

  test('artifact total byte budget stops before reading later files', () => {
    const root = mkdtempSync(join(tmpdir(), 'peri-adlc-budget-'))
    roots.push(root)
    const bytesPerFile = MAX_ADLC_TOTAL_ARTIFACT_BYTES / 4
    const outputs = Array.from({ length: 5 }, (_, index) => {
      const path = `large-${index}.bin`
      writeFileSync(join(root, path), '')
      truncateSync(join(root, path), bytesPerFile)
      return { path, sha256: 'sha256:' + 'a'.repeat(64) }
    })
    const result = checkAdlcStage(stage(root, outputs))
    expect(result.blockers.map((item) => item.code)).toContain('required_output_budget_exceeded')
    expect(result.artifacts).toHaveLength(4)
    expect(result.artifacts.some((item) => item.path === 'large-4.bin')).toBe(false)
    expect(result.pending).toContain('large-4.bin')
  })

  test('assessment requires current-run host identity and excludes participating identity', () => {
    const { root, hash } = fixture()
    const base = {
      stage: 'assessment',
      participantIdentities: [{ runId: 'run-current', agentId: 1 }],
      producerIdentity: { runId: 'run-current', agentId: 9 },
    }
    expect(checkAdlcStage(stage(root, [{ path: 'handoff.md', sha256: hash }], base)).ready).toBe(true)
    const participant = checkAdlcStage(stage(root, [{ path: 'handoff.md', sha256: hash }], {
      ...base,
      producerIdentity: { runId: 'run-current', agentId: 1 },
    }))
    expect(participant.blockers.map((item) => item.code)).toContain('assessment_not_independent')
    const staleRun = checkAdlcStage(stage(root, [{ path: 'handoff.md', sha256: hash }], {
      ...base,
      producerIdentity: { runId: 'run-old', agentId: 9 },
    }))
    expect(staleRun.blockers.map((item) => item.code)).toContain('assessment_producer_run_mismatch')
    expect(() => checkAdlcStage(stage(root, [{ path: 'handoff.md', sha256: hash }], {
      ...base,
      producerIdentity: { runId: 'run-current', agentId: '9' as unknown as number },
    }))).toThrow('non-negative safe integer')
  })
})

describe('ADLC recovery planner', () => {
  test('preserves four blocker classes and only emits structured recovery actions', () => {
    const result = planAdlcRecovery({
      packages: [{ id: 'source', status: 'complete', requiredChecksPassed: true, current: fingerprints('same'), previous: fingerprints('same') }],
      blockers: [
        { kind: 'product_gap', code: 'missing-check', message: 'check missing', packageId: 'source' },
        { kind: 'capability', code: 'no-route', message: 'route unavailable' },
        { kind: 'protocol', code: 'bad-input', message: 'input stale' },
        { kind: 'closeout', code: 'drain', message: 'drain incomplete' },
      ],
    })
    expect(result.ok).toBe(true)
    expect(result.planValid).toBe(true)
    expect(result.semanticAcceptance).toBe('not_checked')
    expect(result.blockers.map((item) => item.kind)).toEqual(['product_gap', 'capability', 'protocol', 'closeout'])
    expect(result.reusable).toEqual([])
    expect(result.ready).toEqual([])
    expect(result.actions.map((item) => item.kind)).toEqual(['repair', 'diagnose', 'repair', 'closeout'])
  })

  test('changed input invalidates downstream closure while unchanged complete package is reusable', () => {
    const same = fingerprints('same')
    const changed = fingerprints('changed')
    const request: AdlcPlanRequest = {
      packages: [
        { id: 'source', status: 'complete', requiredChecksPassed: true, current: same, previous: same },
        { id: 'changed', status: 'complete', requiredChecksPassed: true, current: changed, previous: same },
        { id: 'sink', status: 'complete', requiredChecksPassed: true, dependencies: ['changed'], current: same, previous: same },
        { id: 'partial', status: 'incomplete', requiredChecksPassed: false, current: same, previous: same },
      ],
      invalidPackageIds: ['changed'],
    }
    const result = planAdlcRecovery(request)
    expect(result.reusable).toEqual(['source'])
    expect(result.ready).toEqual(['changed', 'partial'])
    expect(result.pending).toEqual(['changed', 'partial', 'sink'])
    expect(result.actions).toEqual([
      { kind: 'rerun', packageId: 'changed', reason: 'package or dependency fingerprint changed' },
      { kind: 'start', packageId: 'partial', reason: 'package checkpoint is pending' },
    ])
  })

  test('pending source prevents complete downstream package from being reused', () => {
    const same = fingerprints('same')
    const result = planAdlcRecovery({
      packages: [
        { id: 'sink', status: 'complete', requiredChecksPassed: true, dependencies: ['source'], current: same, previous: same },
        { id: 'source', status: 'pending', requiredChecksPassed: false, current: same, previous: same },
      ],
    })
    expect(result.reusable).toEqual([])
    expect(result.ready).toEqual(['source'])
    expect(result.pending).toEqual(['sink', 'source'])
    expect(result.actions).toEqual([{ kind: 'start', packageId: 'source', reason: 'package checkpoint is pending' }])
  })

  test('complete package without previous fingerprint is validated but never started', () => {
    const result = planAdlcRecovery({
      packages: [{ id: 'new', status: 'complete', requiredChecksPassed: true, current: fingerprints('new') }],
    })
    expect(result.ready).toEqual([])
    expect(result.pending).toEqual([])
    expect(result.reusable).toEqual([])
    expect(result.actions).toEqual([])
  })

  test('blocked complete package cannot be reused or started', () => {
    const same = fingerprints('blocked')
    const result = planAdlcRecovery({
      packages: [{ id: 'blocked', status: 'blocked', requiredChecksPassed: true, current: same, previous: same }],
    })
    expect(result.reusable).toEqual([])
    expect(result.ready).toEqual([])
    expect(result.pending).toEqual(['blocked'])
    expect(result.actions).toEqual([])
  })

  test('independent assessment is the sole missing action and package order does not change ready result', () => {
    const same = fingerprints('same')
    const assessor = { id: 'assess', status: 'pending' as const, assessment: true, requiredChecksPassed: false, dependencies: ['impl'], current: same, previous: same }
    const impl = { id: 'impl', status: 'complete' as const, requiredChecksPassed: true, current: same, previous: same }
    const forward = planAdlcRecovery({ packages: [impl, assessor] })
    const reverse = planAdlcRecovery({ packages: [assessor, impl] })
    expect(forward.reusable).toEqual(['impl'])
    expect(forward.ready).toEqual(['assess'])
    expect(forward.actions).toEqual([{ kind: 'assess', packageId: 'assess', reason: 'independent assessment is the sole missing result' }])
    expect(reverse.ready).toEqual(forward.ready)
    expect(() => planAdlcRecovery({ packages: [{ ...impl, id: 'a', dependencies: ['b'] }, { ...impl, id: 'b', dependencies: ['a'] }] })).toThrow('cycle')
  })
})
