#!/usr/bin/env node

/**
 * Replays the filesystem-boundary helper through the packaged CLI.
 *
 * This is an experiment only. It creates and removes private temporary Git
 * fixtures, never reads the user's repository contents, and records only
 * scanner counters and the small set of expected boundary outcomes.
 */
import { createHash } from 'node:crypto'
import { execFileSync, spawnSync } from 'node:child_process'
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { performance } from 'node:perf_hooks'
import { fileURLToPath } from 'node:url'

const DEFAULT_REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..')
const DEFAULT_GENERATED_COUNT = 10_000
const UUIDS = {
  populated: '01923456-789a-7abc-8def-0123456789ab',
  empty: '01923456-789a-7abc-8def-0123456789ac',
}

function parseArgs(argv) {
  const args = { repoRoot: DEFAULT_REPO_ROOT, generatedCount: DEFAULT_GENERATED_COUNT, out: undefined, dist: undefined }
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]
    if (arg === '--repo-root') args.repoRoot = resolve(argv[++i])
    else if (arg === '--dist') args.dist = resolve(argv[++i])
    else if (arg === '--generated-count') args.generatedCount = Number(argv[++i])
    else if (arg === '--out') args.out = resolve(argv[++i])
    else if (arg === '--help' || arg === '-h') {
      console.log('usage: node boundary-replay.mjs [--repo-root PATH] [--dist PATH] [--generated-count N] [--out PATH]')
      process.exit(0)
    } else throw new Error(`unknown argument: ${arg}`)
  }
  if (!Number.isSafeInteger(args.generatedCount) || args.generatedCount < 0 || args.generatedCount > 10_000) {
    throw new Error('--generated-count must be an integer from 0 through 10000')
  }
  args.dist ??= join(args.repoRoot, 'npm-packages/@peri-workflow/dist/peri-workflow.js')
  return args
}

function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

function git(repo, env, ...args) {
  execFileSync('git', args, { cwd: repo, env, stdio: 'ignore' })
}

function fixture(parent, generatedCount, id, env) {
  const repo = mkdtempSync(join(parent, 'repo-'))
  mkdirSync(join(repo, 'src'), { recursive: true })
  mkdirSync(join(repo, 'target'), { recursive: true })
  mkdirSync(join(repo, 'audit'), { recursive: true })
  mkdirSync(join(repo, 'protected'), { recursive: true })
  mkdirSync(join(repo, '.peri/adlc/artifacts'), { recursive: true })
  writeFileSync(join(repo, '.gitignore'), 'target/\naudit/\nprotected/\n.peri/\n')
  writeFileSync(join(repo, 'src/main.rs'), 'fn main() { println!("baseline"); }\n')
  writeFileSync(join(repo, 'src/config.toml'), 'mode = "fixture"\n')
  writeFileSync(join(repo, 'audit/audit.log'), 'audit baseline\n')
  writeFileSync(join(repo, 'protected/secret.txt'), 'SECRET-0000\n')
  for (let i = 0; i < generatedCount; i += 1) {
    writeFileSync(join(repo, 'target', `generated-${String(i).padStart(5, '0')}.txt`), `generated-${i}\n`)
  }
  git(repo, env, 'init', '-q')
  git(repo, env, 'config', 'user.name', 'ADLC boundary replay')
  git(repo, env, 'config', 'user.email', 'boundary-replay@example.invalid')
  git(repo, env, 'add', '.gitignore', 'src')
  git(repo, env, 'commit', '-qm', 'fixture baseline')
  // Deliberately dirty before the boundary snapshot; unchanged compare must preserve it.
  writeFileSync(join(repo, 'src/main.rs'), 'fn main() { println!("preexisting dirty"); }\n')
  return {
    repo,
    baselinePath: `.peri/adlc/artifacts/boundary-${id}.json`,
    requestRoot: mkdtempSync(join(parent, 'requests-')),
  }
}

function invoke(dist, fixtureData, env, operation, request, label) {
  const requestPath = join(fixtureData.requestRoot, `${label}.json`)
  writeFileSync(requestPath, JSON.stringify(request))
  const started = performance.now()
  const result = spawnSync(process.execPath, [dist, 'boundary', operation, requestPath], {
    cwd: fixtureData.repo,
    env,
    encoding: 'utf8',
    timeout: 120_000,
  })
  const wallMs = Math.round(performance.now() - started)
  let json
  try {
    json = result.stdout ? JSON.parse(result.stdout) : undefined
  } catch {
    json = undefined
  }
  return {
    label,
    operation,
    exitCode: result.status ?? 1,
    wallMs,
    ok: json?.ok ?? false,
    baselineId: json?.baselineId,
    fingerprint: json?.fingerprint,
    readFiles: json?.readFiles,
    readBytes: json?.readBytes,
    scannerElapsedMs: json?.elapsedMs,
    coverage: json?.coverage,
    changes: json?.changes,
  }
}

function requestFor(fixtureData, id) {
  return {
    schemaVersion: 1,
    operation: 'snapshot',
    baselineId: id,
    repoRoot: realpathSync(fixtureData.repo),
    cwd: realpathSync(fixtureData.repo),
    limits: { maxEntries: 1000, maxBytes: 256 * 1024 * 1024, maxDepth: 32, deadlineMs: 30_000 },
    baselinePath: fixtureData.baselinePath,
    pathAllowlist: ['src', 'target'],
    generatedRoots: [{ path: 'target', kind: 'host-generated' }],
  }
}

function runScenario(dist, parent, env, generatedCount, id, name) {
  const fixtureData = fixture(parent, generatedCount, id, env)
  try {
    const base = requestFor(fixtureData, id)
    const snapshot = invoke(dist, fixtureData, env, 'snapshot', base, `${name}-snapshot`)
    const unchanged = invoke(
      dist,
      fixtureData,
      env,
      'compare',
      { ...base, operation: 'compare', expectedBaselineFingerprint: snapshot.fingerprint },
      `${name}-unchanged`,
    )

    writeFileSync(join(fixtureData.repo, 'src/main.rs'), 'fn main() { println!("allowlisted"); }\n')
    const allowlisted = invoke(
      dist,
      fixtureData,
      env,
      'compare',
      { ...base, operation: 'compare', expectedBaselineFingerprint: snapshot.fingerprint },
      `${name}-allowlisted`,
    )

    writeFileSync(join(fixtureData.repo, 'protected/secret.txt'), 'SECRET-1111\n')
    const ignoredChanged = invoke(
      dist,
      fixtureData,
      env,
      'compare',
      { ...base, operation: 'compare', expectedBaselineFingerprint: snapshot.fingerprint },
      `${name}-ignored-changed`,
    )

    const missingBaseline = invoke(
      dist,
      fixtureData,
      env,
      'compare',
      { ...base, operation: 'compare', baselinePath: '.peri/adlc/artifacts/missing.json', expectedBaselineFingerprint: snapshot.fingerprint },
      `${name}-missing-baseline`,
    )
    const wrongBaseline = invoke(
      dist,
      fixtureData,
      env,
      'compare',
      { ...base, operation: 'compare', expectedBaselineFingerprint: `sha256:${'0'.repeat(64)}` },
      `${name}-wrong-baseline`,
    )
    return {
      generatedFileCount: generatedCount,
      snapshot,
      unchanged,
      allowlisted,
      ignoredChanged,
      missingBaseline,
      wrongBaseline,
    }
  } finally {
    rmSync(fixtureData.requestRoot, { recursive: true, force: true })
    rmSync(fixtureData.repo, { recursive: true, force: true })
  }
}

function main() {
  const args = parseArgs(process.argv.slice(2))
  const dist = realpathSync(args.dist)
  const parent = mkdtempSync(join(tmpdir(), 'adlc-boundary-replay-'))
  const home = join(parent, 'home')
  mkdirSync(home, { recursive: true })
  const env = {
    ...process.env,
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_TERMINAL_PROMPT: '0',
  }
  const started = new Date().toISOString()
  try {
    const populated = runScenario(dist, parent, env, args.generatedCount, UUIDS.populated, 'populated')
    const empty = runScenario(dist, parent, env, 0, UUIDS.empty, 'empty')
    const populatedScan = populated.snapshot
    const emptyScan = empty.snapshot
    const report = {
      schemaVersion: 1,
      experiment: 'adlc-boundary-replay',
      startedAt: started,
      finishedAt: new Date().toISOString(),
      inputs: {
        repoRootProvided: true,
        artifact: 'npm-packages/@peri-workflow/dist/peri-workflow.js',
        artifactSha256: sha256File(dist),
        generatedFileCount: args.generatedCount,
        emptyComparisonCount: 0,
        limits: { maxEntries: 1000, maxBytes: 256 * 1024 * 1024, maxDepth: 32, deadlineMs: 30_000 },
      },
      scenarios: { populated, empty },
      observations: {
        generatedFilesDoNotEnterScan: populatedScan.readFiles === emptyScan.readFiles && populatedScan.readBytes === emptyScan.readBytes,
        normalUnchangedPreexistingDirtyPasses: populated.unchanged.ok && populated.unchanged.exitCode === 0,
        allowlistedModificationPasses: populated.allowlisted.ok && populated.allowlisted.exitCode === 0,
        ignoredSameLengthModificationCaptured: !populated.ignoredChanged.ok && populated.ignoredChanged.changes?.outOfScope?.includes('protected/secret.txt') === true,
        missingBaselineNonZero: populated.missingBaseline.exitCode !== 0,
        wrongBaselineNonZero: populated.wrongBaseline.exitCode !== 0,
        populatedScanner: { readFiles: populatedScan.readFiles, readBytes: populatedScan.readBytes, elapsedMs: populatedScan.scannerElapsedMs, cliWallMs: populatedScan.wallMs },
        emptyScanner: { readFiles: emptyScan.readFiles, readBytes: emptyScan.readBytes, elapsedMs: emptyScan.scannerElapsedMs, cliWallMs: emptyScan.wallMs },
      },
      limitations: [
        'This is a synthetic fixture replay through the packaged CLI, not a measurement of real task efficiency.',
        'The generated root is explicitly authorized and its contents are intentionally not enumerated by the boundary scanner.',
        'No historical 820k-file corpus or speedup claim is inferred from this run.',
        'Re-run after the final Rust/package build to refresh artifactSha256 and observations.',
      ],
    }
    if (args.out) writeFileSync(args.out, `${JSON.stringify(report, null, 2)}\n`)
    console.log(JSON.stringify(report, null, 2))
  } finally {
    rmSync(parent, { recursive: true, force: true })
  }
}

main()
