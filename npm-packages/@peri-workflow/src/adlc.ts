/**
 * Small, deterministic ADLC gates.
 *
 * This module observes files and Main-agent supplied facts. It never starts a
 * workflow, changes engine state, or decides semantic product acceptance.
 */
import { createHash } from 'node:crypto'
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readSync,
  realpathSync,
  statSync,
} from 'node:fs'
import { isAbsolute, join, relative, sep } from 'node:path'

export const ADLC_SCHEMA_VERSION = 1
export const MAX_ADLC_REQUEST_BYTES = 1024 * 1024
export const MAX_ADLC_ARTIFACTS = 256
export const MAX_ADLC_ARTIFACT_BYTES = 16 * 1024 * 1024
export const MAX_ADLC_TOTAL_ARTIFACT_BYTES = 64 * 1024 * 1024

export type AdlcBlockerKind = 'product_gap' | 'capability' | 'protocol' | 'closeout'

export type AdlcBlocker = {
  kind: AdlcBlockerKind
  code: string
  message: string
  packageId?: string
  path?: string
}

export type AdlcAction = {
  kind: 'repair' | 'diagnose' | 'closeout' | 'rerun' | 'start' | 'assess'
  reason: string
  packageId?: string
}

export type AdlcArtifactRequirement = {
  path: string
  sha256: string
  label?: string
}

export type AdlcProducerIdentity = {
  runId: string
  agentId: number
}

export type AdlcStageRequest = {
  schemaVersion?: number
  adlcDeclared: boolean
  stage: string
  canonicalRoot: string
  currentRunId: string
  requiredOutputs: AdlcArtifactRequirement[]
  participantIdentities?: AdlcProducerIdentity[]
  producerIdentity?: AdlcProducerIdentity
}

export type AdlcArtifactEvidence = {
  path: string
  expectedSha256: string
  actualSha256?: string
  bytes?: number
  valid: boolean
}

export type AdlcStageResult = {
  schema: 'peri.adlc/stage-result-v1'
  ok: boolean
  ready: boolean
  adlcDeclared: boolean
  stage: string
  semanticAcceptance: 'not_checked'
  reusable: string[]
  pending: string[]
  blockers: AdlcBlocker[]
  actions: AdlcAction[]
  artifacts: AdlcArtifactEvidence[]
}

export type AdlcFingerprintSnapshot = {
  contract: string
  input: string
  dependencies: Record<string, string>
  verifiedArtifacts: Array<{ path: string; sha256: string }>
}

export type AdlcPackage = {
  id: string
  status: 'complete' | 'incomplete' | 'blocked' | 'pending' | 'invalid'
  dependencies?: string[]
  requiredChecksPassed: boolean
  sourceCompileGatePassed?: boolean
  assessment?: boolean
  current: AdlcFingerprintSnapshot
  previous?: AdlcFingerprintSnapshot
}

export type AdlcPlanRequest = {
  schemaVersion?: number
  packages: AdlcPackage[]
  blockers?: AdlcBlocker[]
  invalidPackageIds?: string[]
}

export type AdlcPackagePlan = {
  id: string
  status: AdlcPackage['status']
  dependencies: string[]
  invalidated: boolean
  validated: boolean
  reusable: boolean
  ready: boolean
  fingerprintsMatch: boolean
  verifiedArtifactCount: number
  reason?: string
}

export type AdlcPlanResult = {
  schema: 'peri.adlc/plan-v1'
  ok: boolean
  planValid: boolean
  semanticAcceptance: 'not_checked'
  ready: string[]
  reusable: string[]
  pending: string[]
  blockers: AdlcBlocker[]
  actions: AdlcAction[]
  packages: AdlcPackagePlan[]
}

class AdlcProtocolError extends Error {}

function fail(message: string): never {
  throw new AdlcProtocolError(message)
}

function boundedString(value: unknown, name: string, max = 1024): string {
  if (typeof value !== 'string' || value.length === 0 || value.length > max) {
    fail(`${name} must be a non-empty string of at most ${max} bytes`)
  }
  return value
}

function ensureSchema(value: unknown): void {
  if (value !== undefined && value !== ADLC_SCHEMA_VERSION) fail('unsupported ADLC schemaVersion')
}

function normalizeSha(value: unknown, name: string): string {
  const raw = boundedString(value, name, 71).toLowerCase()
  const hex = raw.startsWith('sha256:') ? raw.slice('sha256:'.length) : raw
  if (!/^[0-9a-f]{64}$/.test(hex)) fail(`${name} must be sha256:<64 lowercase hex>`)
  return `sha256:${hex}`
}

function agentId(value: unknown, name: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    fail(`${name} must be a non-negative safe integer`)
  }
  return value
}

function normalizeRelativePath(value: unknown, name: string): string {
  const raw = boundedString(value, name, 512).replaceAll('\\', '/')
  if (isAbsolute(raw)) fail(`${name} must be relative to canonicalRoot`)
  const parts = raw.split('/')
  if (parts.length === 0 || parts.some((part) => part === '' || part === '.' || part === '..')) {
    fail(`${name} contains unsafe path components`)
  }
  return parts.join('/')
}

function within(root: string, candidate: string): boolean {
  const child = relative(root, candidate)
  return child === '' || (!child.startsWith('..' + sep) && child !== '..' && !isAbsolute(child))
}

function validateArtifactRequirements(value: unknown): AdlcArtifactRequirement[] {
  if (!Array.isArray(value)) fail('requiredOutputs must be an array')
  if (value.length > MAX_ADLC_ARTIFACTS) fail(`requiredOutputs exceeds ${MAX_ADLC_ARTIFACTS} artifacts`)
  const seen = new Set<string>()
  return value.map((raw, index) => {
    if (!raw || typeof raw !== 'object') fail(`requiredOutputs[${index}] must be an object`)
    const item = raw as Record<string, unknown>
    const path = normalizeRelativePath(item.path, `requiredOutputs[${index}].path`)
    if (seen.has(path)) fail(`requiredOutputs contains duplicate path: ${path}`)
    seen.add(path)
    return {
      path,
      sha256: normalizeSha(item.sha256, `requiredOutputs[${index}].sha256`),
      ...(item.label === undefined ? {} : { label: boundedString(item.label, `requiredOutputs[${index}].label`, 256) }),
    }
  })
}

function addBlocker(
  blockers: AdlcBlocker[],
  kind: AdlcBlockerKind,
  code: string,
  message: string,
  extra: Pick<AdlcBlocker, 'path' | 'packageId'> = {},
): void {
  blockers.push({ kind, code, message, ...extra })
}

function checkPathNoSymlink(root: string, path: string): string {
  let cursor = root
  for (const part of path.split('/')) {
    cursor = join(cursor, part)
    const info = lstatSync(cursor)
    if (info.isSymbolicLink()) fail(`required output is a symlink: ${path}`)
  }
  const canonical = realpathSync(cursor)
  if (!within(root, canonical)) fail(`required output escapes canonicalRoot: ${path}`)
  return canonical
}

function readStableRegularFile(path: string, maxBytes: number): { bytes: number; content: Buffer } {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const before = fstatSync(fd)
    if (!before.isFile()) throw new Error('not a regular file')
    const bytes = Number(before.size)
    if (!Number.isSafeInteger(bytes) || bytes <= 0) throw new Error('file is empty or too large')
    if (bytes > maxBytes) throw new Error(`file exceeds ${maxBytes} bytes`)
    const content = Buffer.alloc(bytes)
    let offset = 0
    while (offset < bytes) {
      const read = readSync(fd, content, offset, bytes - offset, null)
      if (read === 0) throw new Error('file truncated while being read')
      offset += read
    }
    const after = fstatSync(fd)
    if (before.dev !== after.dev || before.ino !== after.ino || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
      throw new Error('file changed while being read')
    }
    return { bytes, content }
  } finally {
    closeSync(fd)
  }
}

function validateStageRequest(request: AdlcStageRequest): {
  root: string
  outputs: AdlcArtifactRequirement[]
} {
  if (!request || typeof request !== 'object') fail('ADLC stage request must be an object')
  ensureSchema(request.schemaVersion)
  if (typeof request.adlcDeclared !== 'boolean') fail('adlcDeclared must be boolean')
  boundedString(request.stage, 'stage', 128)
  const currentRunId = boundedString(request.currentRunId, 'currentRunId', 256)
  if (!currentRunId) fail('currentRunId is required')
  if (typeof request.canonicalRoot !== 'string' || !isAbsolute(request.canonicalRoot)) {
    fail('canonicalRoot must be an absolute path')
  }
  const root = realpathSync(request.canonicalRoot)
  if (!statSync(root).isDirectory()) fail('canonicalRoot must be a directory')
  const outputs = validateArtifactRequirements(request.requiredOutputs)
  if (request.participantIdentities !== undefined && !Array.isArray(request.participantIdentities)) {
    fail('participantIdentities must be an array')
  }
  for (const [index, participant] of (request.participantIdentities ?? []).entries()) {
    if (!participant || typeof participant !== 'object') fail(`participantIdentities[${index}] must be an object`)
    boundedString(participant.runId, `participantIdentities[${index}].runId`, 256)
    agentId(participant.agentId, `participantIdentities[${index}].agentId`)
  }
  if (request.producerIdentity !== undefined) {
    const producer = request.producerIdentity
    if (!producer || typeof producer !== 'object') fail('producerIdentity must be an object')
    boundedString(producer.runId, 'producerIdentity.runId', 256)
    agentId(producer.agentId, 'producerIdentity.agentId')
  }
  return { root, outputs }
}

/** Check declared stage artifacts and, for assessment, host-observed identity. */
export function checkAdlcStage(request: AdlcStageRequest): AdlcStageResult {
  const { root, outputs } = validateStageRequest(request)
  const blockers: AdlcBlocker[] = []
  const artifacts: AdlcArtifactEvidence[] = []
  const pending: string[] = []
  let totalBytes = 0

  for (let index = 0; index < outputs.length; index += 1) {
    const output = outputs[index]
    const remainingBytes = MAX_ADLC_TOTAL_ARTIFACT_BYTES - totalBytes
    if (remainingBytes <= 0) {
      addBlocker(blockers, 'protocol', 'required_output_budget_exceeded', `artifact byte budget exhausted before reading: ${output.path}`, { path: output.path })
      pending.push(...outputs.slice(index).map((item) => item.path))
      break
    }
    try {
      const path = checkPathNoSymlink(root, output.path)
      const file = readStableRegularFile(path, Math.min(MAX_ADLC_ARTIFACT_BYTES, remainingBytes))
      totalBytes += file.bytes
      const actualSha256 = `sha256:${createHash('sha256').update(file.content).digest('hex')}`
      const valid = actualSha256 === output.sha256
      artifacts.push({ path: output.path, expectedSha256: output.sha256, actualSha256, bytes: file.bytes, valid })
      if (!valid) {
        pending.push(output.path)
        addBlocker(blockers, 'protocol', 'required_output_stale', `required output hash does not match: ${output.path}`, { path: output.path })
      } else {
        // `reusable` is assembled below from this evidence.
      }
    } catch (error) {
      pending.push(output.path)
      const message = (error as Error).message
      const budgetExceeded = message.includes('exceeds')
      addBlocker(blockers, 'protocol', budgetExceeded ? 'required_output_budget_exceeded' : 'required_output_invalid', `${output.path}: ${message}`, { path: output.path })
      artifacts.push({ path: output.path, expectedSha256: output.sha256, valid: false })
      // A failed bounded read may already have consumed bytes before its
      // post-read identity check. Stop this pass rather than pretending that
      // the remaining budget is still available to later artifacts.
      pending.push(...outputs.slice(index + 1).map((item) => item.path))
      break
    }
  }

  if (request.stage === 'assessment' && request.adlcDeclared) {
    const producer = request.producerIdentity
    if (!producer) {
      addBlocker(blockers, 'protocol', 'assessment_producer_missing', 'assessment requires host-observed producer identity')
    } else if (producer.runId !== request.currentRunId) {
      addBlocker(blockers, 'protocol', 'assessment_producer_run_mismatch', 'assessment producer is from another run')
    } else if (!request.participantIdentities) {
      addBlocker(blockers, 'protocol', 'assessment_participants_missing', 'assessment participant identities are required')
    } else if (request.participantIdentities.some((participant) => participant.runId === producer.runId && participant.agentId === producer.agentId)) {
      addBlocker(blockers, 'protocol', 'assessment_not_independent', 'assessment producer is a participant in this run')
    }
  }

  const reusable = artifacts.filter((artifact) => artifact.valid).map((artifact) => artifact.path)
  if (request.adlcDeclared && outputs.length === 0) {
    addBlocker(blockers, 'protocol', 'required_outputs_empty', 'ADLC stage has no declared required outputs')
  }
  const actions: AdlcAction[] = blockers.map((blocker) => ({
    kind: blocker.code.startsWith('assessment_') ? 'assess' : 'repair',
    reason: blocker.message,
    ...(blocker.path ? { packageId: blocker.path } : {}),
  }))
  const ready = blockers.length === 0
  return {
    schema: 'peri.adlc/stage-result-v1',
    ok: ready,
    ready,
    adlcDeclared: request.adlcDeclared,
    stage: request.stage,
    semanticAcceptance: 'not_checked',
    reusable,
    pending,
    blockers,
    actions,
    artifacts,
  }
}

function snapshot(value: unknown, name: string): AdlcFingerprintSnapshot {
  if (!value || typeof value !== 'object') fail(`${name} must be an object`)
  const raw = value as Record<string, unknown>
  const dependencies = raw.dependencies
  if (!dependencies || typeof dependencies !== 'object' || Array.isArray(dependencies)) fail(`${name}.dependencies must be an object`)
  const verifiedArtifacts = raw.verifiedArtifacts
  if (!Array.isArray(verifiedArtifacts) || verifiedArtifacts.length > MAX_ADLC_ARTIFACTS) fail(`${name}.verifiedArtifacts is invalid or too large`)
  const normalizedArtifacts = verifiedArtifacts.map((item, index) => {
    if (!item || typeof item !== 'object') fail(`${name}.verifiedArtifacts[${index}] must be an object`)
    const record = item as Record<string, unknown>
    return {
      path: normalizeRelativePath(record.path, `${name}.verifiedArtifacts[${index}].path`),
      sha256: normalizeSha(record.sha256, `${name}.verifiedArtifacts[${index}].sha256`),
    }
  }).sort((a, b) => a.path.localeCompare(b.path))
  return {
    contract: boundedString(raw.contract, `${name}.contract`, 512),
    input: boundedString(raw.input, `${name}.input`, 512),
    dependencies: Object.fromEntries(Object.entries(dependencies).sort(([a], [b]) => a.localeCompare(b)).map(([key, value]) => [boundedString(key, `${name}.dependencies key`, 256), boundedString(value, `${name}.dependencies.${key}`, 512)])),
    verifiedArtifacts: normalizedArtifacts,
  }
}

function snapshotsEqual(a: AdlcFingerprintSnapshot, b: AdlcFingerprintSnapshot): boolean {
  return a.contract === b.contract && a.input === b.input &&
    JSON.stringify(a.dependencies) === JSON.stringify(b.dependencies) &&
    JSON.stringify(a.verifiedArtifacts) === JSON.stringify(b.verifiedArtifacts)
}

function validatePlanRequest(request: AdlcPlanRequest): AdlcPackage[] {
  if (!request || typeof request !== 'object') fail('ADLC plan request must be an object')
  ensureSchema(request.schemaVersion)
  if (!Array.isArray(request.packages) || request.packages.length === 0 || request.packages.length > MAX_ADLC_ARTIFACTS) {
    fail(`packages must contain 1-${MAX_ADLC_ARTIFACTS} entries`)
  }
  const ids = new Set<string>()
  const packages = request.packages.map((raw, index) => {
    if (!raw || typeof raw !== 'object') fail(`packages[${index}] must be an object`)
    const item = raw as AdlcPackage
    const id = boundedString(item.id, `packages[${index}].id`, 256)
    if (ids.has(id)) fail(`duplicate package id: ${id}`)
    ids.add(id)
    const status = item.status
    if (!['complete', 'incomplete', 'blocked', 'pending', 'invalid'].includes(status)) fail(`packages[${index}].status is invalid`)
    if (typeof item.requiredChecksPassed !== 'boolean') fail(`packages[${index}].requiredChecksPassed must be boolean`)
    if (item.sourceCompileGatePassed !== undefined && typeof item.sourceCompileGatePassed !== 'boolean') fail(`packages[${index}].sourceCompileGatePassed must be boolean`)
    if (item.assessment !== undefined && typeof item.assessment !== 'boolean') fail(`packages[${index}].assessment must be boolean`)
    const dependencies = item.dependencies ?? []
    if (!Array.isArray(dependencies) || dependencies.some((dependency) => typeof dependency !== 'string')) fail(`packages[${index}].dependencies must be string[]`)
    return {
      ...item,
      id,
      dependencies: [...new Set(dependencies)].sort(),
      current: snapshot(item.current, `packages[${index}].current`),
      ...(item.previous === undefined ? {} : { previous: snapshot(item.previous, `packages[${index}].previous`) }),
    }
  })
  for (const item of packages) {
    for (const dependency of item.dependencies ?? []) if (!ids.has(dependency)) fail(`package ${item.id} references unknown dependency ${dependency}`)
  }
  return packages
}

/** Build a deterministic recovery plan from Main-observed package facts only. */
export function planAdlcRecovery(request: AdlcPlanRequest): AdlcPlanResult {
  const packages = validatePlanRequest(request)
  const inputBlockers = request.blockers ?? []
  if (!Array.isArray(inputBlockers)) fail('blockers must be an array')
  const blockers = inputBlockers.map((blocker, index) => {
    if (!blocker || typeof blocker !== 'object') fail(`blockers[${index}] must be an object`)
    if (!['product_gap', 'capability', 'protocol', 'closeout'].includes(blocker.kind)) fail(`blockers[${index}].kind is invalid`)
    return {
      kind: blocker.kind,
      code: boundedString(blocker.code, `blockers[${index}].code`, 256),
      message: boundedString(blocker.message, `blockers[${index}].message`, 2048),
      ...(blocker.packageId === undefined ? {} : { packageId: boundedString(blocker.packageId, `blockers[${index}].packageId`, 256) }),
      ...(blocker.path === undefined ? {} : { path: boundedString(blocker.path, `blockers[${index}].path`, 512) }),
    }
  })
  const byId = new Map(packages.map((item) => [item.id, item]))
  if (request.invalidPackageIds !== undefined && !Array.isArray(request.invalidPackageIds)) fail('invalidPackageIds must be an array')
  const invalidated = new Set<string>((request.invalidPackageIds ?? []).map((id) => boundedString(id, 'invalidPackageIds[]', 256)))
  for (const id of invalidated) if (!byId.has(id)) fail(`invalidPackageIds references unknown package ${id}`)
  const blockedByObservation = new Set<string>()
  for (const blocker of blockers) {
    if (blocker.packageId !== undefined) {
      if (!byId.has(blocker.packageId)) fail(`blocker references unknown package ${blocker.packageId}`)
      blockedByObservation.add(blocker.packageId)
    }
  }
  for (const item of packages) if (item.status === 'blocked') blockedByObservation.add(item.id)
  const blockedClosure = new Set(blockedByObservation)
  let blockedChanged = true
  while (blockedChanged) {
    blockedChanged = false
    for (const item of packages) {
      if (item.dependencies?.some((dependency) => blockedClosure.has(dependency)) && !blockedClosure.has(item.id)) {
        blockedClosure.add(item.id)
        blockedChanged = true
      }
    }
  }
  const visit = new Set<string>()
  const visited = new Set<string>()
  const visitPackage = (id: string): void => {
    if (visit.has(id)) fail(`package dependency cycle includes ${id}`)
    if (visited.has(id)) return
    visit.add(id)
    for (const dependency of byId.get(id)!.dependencies ?? []) visitPackage(dependency)
    visit.delete(id)
    visited.add(id)
  }
  for (const item of packages) visitPackage(item.id)
  for (const item of packages) {
    if (item.status === 'invalid' || (item.previous !== undefined && !snapshotsEqual(item.current, item.previous))) invalidated.add(item.id)
    if (item.sourceCompileGatePassed === false) invalidated.add(item.id)
  }
  let changed = true
  while (changed) {
    changed = false
    for (const item of packages) {
      if (item.dependencies?.some((dependency) => invalidated.has(dependency)) && !invalidated.has(item.id)) {
        invalidated.add(item.id)
        changed = true
      }
    }
  }

  // Validation is a dependency-closed fact. A complete package with a
  // missing/failed check is not validated, and a sink cannot become validated
  // merely because the sink itself still has an old successful snapshot.
  const validated = new Set<string>()
  let validatedChanged = true
  while (validatedChanged) {
    validatedChanged = false
    for (const item of packages) {
      const ownValid = item.status === 'complete' && item.requiredChecksPassed && item.sourceCompileGatePassed !== false && !invalidated.has(item.id) && !blockedClosure.has(item.id)
      const dependenciesValid = (item.dependencies ?? []).every((dependency) => validated.has(dependency))
      if (ownValid && dependenciesValid && !validated.has(item.id)) {
        validated.add(item.id)
        validatedChanged = true
      }
    }
  }

  // Reuse is stronger than validation: the current and previous fingerprints
  // must match through the dependency closure as well.
  const reusable = new Set<string>()
  let reusableChanged = true
  while (reusableChanged) {
    reusableChanged = false
    for (const item of packages) {
      const ownEvidenceUnchanged = validated.has(item.id) && item.previous !== undefined && snapshotsEqual(item.current, item.previous)
      const dependenciesReusable = (item.dependencies ?? []).every((dependency) => reusable.has(dependency))
      if (ownEvidenceUnchanged && dependenciesReusable && !reusable.has(item.id)) {
        reusable.add(item.id)
        reusableChanged = true
      }
    }
  }

  const globalBlocker = blockers.some((blocker) => blocker.packageId === undefined)
  // `ready` means ready to start/re-run now: it is not validated yet, all
  // direct dependencies are validated, and no global/direct blocker applies.
  const ready = new Set<string>()
  if (!globalBlocker) {
    for (const item of packages) {
      const dependenciesValid = (item.dependencies ?? []).every((dependency) => validated.has(dependency))
      if (!validated.has(item.id) && !blockedClosure.has(item.id) && dependenciesValid) ready.add(item.id)
    }
  }

  // Pending intentionally includes ready items: it is the full not-yet-
  // validated set, while `ready` is the subset that can start immediately.
  const pending = packages.filter((item) => !validated.has(item.id)).map((item) => item.id).sort()
  const packagePlans: AdlcPackagePlan[] = packages.map((item) => ({
    id: item.id,
    status: item.status,
    dependencies: item.dependencies ?? [],
    invalidated: invalidated.has(item.id),
    validated: validated.has(item.id),
    reusable: reusable.has(item.id),
    ready: ready.has(item.id),
    fingerprintsMatch: item.previous !== undefined && snapshotsEqual(item.current, item.previous),
    verifiedArtifactCount: item.current.verifiedArtifacts.length,
    ...(!validated.has(item.id) ? { reason: globalBlocker ? 'global_blocker' : blockedClosure.has(item.id) ? 'observed_blocker' : invalidated.has(item.id) ? 'fingerprint_or_dependency_changed' : item.sourceCompileGatePassed === false ? 'source_compile_gate_failed' : 'required_checks_pending' } : {}),
  }))
  const actions: AdlcAction[] = []
  for (const blocker of blockers) {
    actions.push({ kind: blocker.kind === 'capability' ? 'diagnose' : blocker.kind === 'closeout' ? 'closeout' : 'repair', reason: blocker.message, ...(blocker.packageId ? { packageId: blocker.packageId } : {}) })
  }
  const assessmentOnly = blockers.length === 0 && packages.filter((item) => item.assessment && ready.has(item.id)).length === 1 && packages.filter((item) => !item.assessment && !validated.has(item.id)).length === 0
  if (assessmentOnly) {
    const assessor = packages.find((item) => item.assessment && ready.has(item.id))!
    actions.push({ kind: 'assess', packageId: assessor.id, reason: 'independent assessment is the sole missing result' })
  } else {
    for (const item of packages) {
      if (!ready.has(item.id) || item.assessment) continue
      const shouldRerun = invalidated.has(item.id) || item.sourceCompileGatePassed === false || item.status === 'complete'
      actions.push({ kind: shouldRerun ? 'rerun' : 'start', packageId: item.id, reason: shouldRerun ? item.sourceCompileGatePassed === false ? 'source compile gate failed' : 'package or dependency fingerprint changed' : 'package checkpoint is pending' })
    }
  }
  return {
    schema: 'peri.adlc/plan-v1',
    // `ok` means the deterministic plan was generated. It is not a product
    // completion verdict; pending packages and blockers remain visible below.
    ok: true,
    planValid: true,
    semanticAcceptance: 'not_checked',
    ready: [...ready].sort(),
    reusable: [...reusable].sort(),
    pending,
    blockers,
    actions,
    packages: packagePlans,
  }
}

function readAdlcJson(path: string): unknown {
  const info = lstatSync(path)
  if (!info.isFile() || info.isSymbolicLink()) fail('ADLC request must be a regular non-symlink file')
  if (info.size > MAX_ADLC_REQUEST_BYTES) fail(`ADLC request exceeds ${MAX_ADLC_REQUEST_BYTES} bytes`)
  return JSON.parse(readStableRegularFile(path, MAX_ADLC_REQUEST_BYTES).content.toString('utf8')) as unknown
}

/** CLI adapter; pure helpers above remain usable by tests and Main. */
export function adlcFile(args: string[]): void {
  const operation = args[0]
  const requestPath = args[1]
  if ((operation !== 'check-stage' && operation !== 'plan') || args.length !== 2 || !requestPath || requestPath.startsWith('--')) {
    console.error('用法：peri-workflow adlc check-stage|plan <request.json>')
    process.exit(1)
  }
  try {
    const request = readAdlcJson(requestPath)
    const result = operation === 'check-stage' ? checkAdlcStage(request as AdlcStageRequest) : planAdlcRecovery(request as AdlcPlanRequest)
    console.log(JSON.stringify(result, null, 2))
    if (!result.ok) process.exit(2)
  } catch (error) {
    console.error(`adlc ${operation} failed: ${(error as Error).message}`)
    process.exit(1)
  }
}
