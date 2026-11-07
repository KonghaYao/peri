import { createHash, randomUUID } from "node:crypto";
import { chmod, cp, mkdir, readFile, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { performance } from "node:perf_hooks";
import { ScenarioIdentity } from "./llm-replay-identity.js";
import { declareReplayAmbientBindings } from "./llm-replay-runtime.js";
import {
  createReplayWorld, FIXTURE_MODEL_ORIGIN, prepareReplaySessions, ReplayTui,
  syntheticRequestEvidence, type ReplayTuiOptions, type ReplayWorld,
} from "./llm-replay-tui.js";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const proxyModule = new URL("../llm-replay/proxy.mjs", import.meta.url).href;
const bindingModule = new URL("../llm-replay/request-bindings.mjs", import.meta.url).href;
const invocationId = process.env.PERI_E2E_REPLAY_INVOCATION ?? `${new Date().toISOString().replace(/[:.]/g, "-")}-${randomUUID().slice(0, 8)}`;

export interface ReplayRequestObservation {
  context: any;
  request: { method: string; target: string; headers: Record<string, string>; body: Buffer };
  expected?: any;
}

export interface ReplayScenarioContext {
  world: ReplayWorld;
  sessionIds: Record<string, string>;
  identity: ScenarioIdentity;
  proxy: any;
  run: any;
  mode: "record" | "replay";
  runId: string;
  upstream?: any;
  bindings: any;
  requests: ReplayRequestObservation[];
  startTui(alias: string, options?: Partial<ReplayTuiOptions>): Promise<ReplayTui>;
  bind(input: { kind: string; alias: string; live: string; anchor: string; owner?: string; recorded?: string }): any;
  observe(name: string, value: unknown): void;
  checked<T>(predicate: (value: T) => boolean): (value: T) => boolean;
  waitForCalls(predicate: (snapshot: any) => boolean, timeoutMs?: number): Promise<any>;
}

export interface ReplayScenarioDefinition {
  id: string;
  title: string;
  sessionNames?: string[];
  configVersion?: string;
  logicalOrigin?: string;
  configure?(world: ReplayWorld): Promise<void> | void;
  upstream?(world: ReplayWorld): Promise<{ url: string; close(): Promise<unknown> | unknown; [key: string]: any }>;
  runOptions?(context: ReplayScenarioContext): Promise<Record<string, any>> | Record<string, any>;
  portableTools?: Record<string, string[]>;
  run(context: ReplayScenarioContext): Promise<unknown>;
}

interface SeedFile { path: string; sha256: string; size: number; mode: number }
interface ScenarioManifest {
  version: 2;
  id: string;
  title: string;
  provenance: { source: "local-fixture-model"; realLlm: false };
  sessionNames: string[];
  seed: { directories: string[]; files: SeedFile[] };
  recordedWorld: Pick<ReplayWorld, "root" | "home" | "work">;
  recordedSessionIds: Record<string, string>;
  configurationFingerprint: string;
  bindings: any[];
  portableResponses: any[];
}

export interface ReplayScenarioReport {
  version: 2;
  id: string;
  title: string;
  mode: "record" | "replay";
  status: "passed" | "failed";
  world: ReplayWorld;
  fixtureDirectory: string;
  timings: { preparationMs: number; proxyBootMs: number; driverMs: number; scenarioMs: number; cleanupMs: number; totalMs: number };
  observations: Array<{ name: string; value: unknown; atMs: number }>;
  proxy?: any;
  error?: { name: string; message: string; stack?: string };
}

function digest(bytes: Buffer): string { return createHash("sha256").update(bytes).digest("hex"); }
function safeRelative(relative: string): void {
  if (path.isAbsolute(relative) || relative.includes("\\") || path.normalize(relative) !== relative
    || relative === ".." || relative.startsWith("../") || !/^(home|work)(\/|$)/.test(relative)) throw new Error(`unsafe seed path: ${relative}`);
}

/** The seed precedes ACP session creation: configuration and working files, never a database relocation. */
async function saveSeed(world: ReplayWorld, directory: string): Promise<ScenarioManifest["seed"]> {
  const seed: ScenarioManifest["seed"] = { directories: [], files: [] };
  await mkdir(path.join(directory, "blobs"), { recursive: true });
  const visit = async (absolute: string, relative: string): Promise<void> => {
    seed.directories.push(relative);
    for (const entry of (await readdir(absolute, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      const source = path.join(absolute, entry.name), target = `${relative}/${entry.name}`;
      if (entry.isDirectory()) await visit(source, target);
      else if (entry.isFile()) {
        const bytes = await readFile(source);
        if (bytes.subarray(0, 16).toString() === "SQLite format 3\0") throw new Error(`seed must precede session database creation: ${target}`);
        if (bytes.includes(Buffer.from(world.root))) throw new Error(`seed contains a recording-only absolute world path: ${target}; use relative fixture configuration`);
        const sha256 = digest(bytes);
        const mode = (await stat(source)).mode & 0o777;
        await writeFile(path.join(directory, "blobs", `${sha256}.bin`), bytes);
        seed.files.push({ path: target, sha256, size: bytes.length, mode });
      } else throw new Error(`seed supports ordinary files and directories only: ${target}`);
    }
  };
  await visit(world.home, "home");
  await visit(world.work, "work");
  return seed;
}

async function restoreSeed(world: ReplayWorld, directory: string, seed: ScenarioManifest["seed"]): Promise<void> {
  // Validate every blob before replacing this run's owned, empty initialization directories.
  const blobs = new Map<string, Buffer>();
  for (const file of seed.files) {
    safeRelative(file.path);
    if (!/^[a-f0-9]{64}$/.test(file.sha256)) throw new Error("invalid seed digest");
    const bytes = await readFile(path.join(directory, "blobs", `${file.sha256}.bin`));
    if (bytes.length !== file.size || digest(bytes) !== file.sha256) throw new Error(`seed integrity mismatch: ${file.path}`);
    blobs.set(file.path, bytes);
  }
  for (const relative of seed.directories) safeRelative(relative);
  await Promise.all([rm(world.home, { recursive: true, force: true }), rm(world.work, { recursive: true, force: true })]);
  for (const relative of seed.directories) await mkdir(path.join(world.root, relative), { recursive: true });
  for (const file of seed.files) {
    await writeFile(path.join(world.root, file.path), blobs.get(file.path)!);
    await chmod(path.join(world.root, file.path), file.mode);
  }
}

/** Explicit record/update writes a new fixture; the default path can only load and replay a sealed fixture. */
export async function runReplayScenario(definition: ReplayScenarioDefinition): Promise<ReplayScenarioReport> {
  if (!/^[A-Z][0-9]{2}$/.test(definition.id)) throw new Error("scenario id must be an explicit catalog id, such as S01");
  const requestedMode = process.env.PERI_E2E_REPLAY_MODE ?? "replay";
  if (requestedMode !== "record" && requestedMode !== "replay") throw new Error("PERI_E2E_REPLAY_MODE must be record or replay");
  const mode = requestedMode;
  if (mode === "record" && process.env.CI && process.env.CI !== "false") throw new Error("CI is replay-only; fixture updates require an explicit local record run");
  const started = performance.now();
  const configurationFingerprint = digest(Buffer.from(JSON.stringify({
    defaultWorld: createReplayWorld.toString(),
    configure: definition.configure?.toString() ?? null,
    configVersion: definition.configVersion ?? null,
  })));
  const fixtureRoot = path.resolve(process.env.PERI_E2E_REPLAY_FIXTURES ?? path.join(projectRoot, "e2e/fixtures/replay-v2"));
  const fixtureDirectory = path.join(fixtureRoot, definition.id);
  const reportRoot = path.resolve(process.env.PERI_E2E_REPLAY_RESULTS ?? path.join(projectRoot, "e2e/results/replay-v2", invocationId));
  const world = await createReplayWorld();
  world.evidence = path.join(reportRoot, definition.id);
  await mkdir(world.evidence, { recursive: true });
  const report: ReplayScenarioReport = {
    version: 2, id: definition.id, title: definition.title, mode, status: "failed", world, fixtureDirectory,
    timings: { preparationMs: 0, proxyBootMs: 0, driverMs: 0, scenarioMs: 0, cleanupMs: 0, totalMs: 0 }, observations: [],
  };
  let proxy: any;
  let upstream: any;
  let staging: string | undefined;
  let lock: string | undefined;
  const lockToken = randomUUID();
  let scenarioStarted: number | undefined;
  let driverFinished = false;
  let primaryFailure = false;
  let currentRun: string | undefined;
  const terminals: ReplayTui[] = [];
  try {
    const { startReplayProxy } = await import(proxyModule);
    const { createRequestBindingAdapter } = await import(bindingModule);
    let manifest: ScenarioManifest;
    if (mode === "record") {
      await mkdir(fixtureRoot, { recursive: true });
      const requestedLock = path.join(fixtureRoot, `.lock-${definition.id}`);
      try { await mkdir(requestedLock); }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
        const owner = await readFile(path.join(requestedLock, "owner.json"), "utf8").catch(() => "owner metadata unavailable");
        throw new Error(`recording lock already exists for ${definition.id}; verify its owner before removing it: ${owner}`);
      }
      lock = requestedLock;
      await writeFile(path.join(lock, "owner.json"), JSON.stringify({ token: lockToken, pid: process.pid, id: definition.id,
        invocation: invocationId, startedAt: new Date().toISOString(), world: world.root }, null, 2));
      staging = path.join(fixtureRoot, `.record-${definition.id}-${randomUUID()}`);
      await mkdir(staging);
      await definition.configure?.(world);
      manifest = {
        version: 2, id: definition.id, title: definition.title,
        provenance: { source: "local-fixture-model", realLlm: false },
        sessionNames: definition.sessionNames ?? ["root"], seed: await saveSeed(world, path.join(staging, "seed")),
        recordedWorld: { root: world.root, home: world.home, work: world.work }, recordedSessionIds: {}, configurationFingerprint, bindings: [], portableResponses: [],
      };
    } else {
      try { manifest = JSON.parse(await readFile(path.join(fixtureDirectory, "scenario.json"), "utf8")); }
      catch (error) { throw new Error(`missing or unreadable sealed fixture ${definition.id}; explicitly record it first`, { cause: error }); }
      if (manifest.version !== 2 || manifest.id !== definition.id) throw new Error("scenario fixture identity/version mismatch");
      if (manifest.configurationFingerprint !== configurationFingerprint) throw new Error("fixture configuration changed or lacks a fingerprint; explicit re-record required");
      if (JSON.stringify(manifest.sessionNames) !== JSON.stringify(definition.sessionNames ?? ["root"])) throw new Error("scenario session topology changed; explicit re-record required");
      await restoreSeed(world, path.join(fixtureDirectory, "seed"), manifest.seed);
    }
    const ids = await prepareReplaySessions(world, manifest.sessionNames.length);
    const sessionIds = Object.fromEntries(manifest.sessionNames.map((alias, index) => [alias, ids[index]!]));
    const identity = new ScenarioIdentity(Object.entries(sessionIds).map(([canonical, live]) => ({ live, canonical })));
    if (mode === "record") manifest.recordedSessionIds = sessionIds;
    currentRun = `${definition.id}-${mode}-${randomUUID()}`;
    const bindings = createRequestBindingAdapter({ mode, runId: currentRun, declarations: manifest.bindings, owner: "world" });
    await declareReplayAmbientBindings(bindings, world);
    const syncOwners = () => {
      for (const owner of identity.ownerBindings()) bindings.declareAndBind({ kind: "session", alias: owner.canonical, live: owner.live, anchor: owner.anchor });
    };
    syncOwners();
    if (mode === "record") {
      if (!definition.upstream) throw new Error("record requires an explicit local fixture upstream");
      upstream = await definition.upstream(world);
      const origin = new URL(upstream.url);
      if (!["127.0.0.1", "localhost", "[::1]"].includes(origin.hostname)) throw new Error("business fixture upstream must be local and synthetic");
    }
    const cassetteDir = path.join(staging ?? fixtureDirectory, "cassette");
    const proxyBootStarted = performance.now();
    proxy = await startReplayProxy({ mode, cassetteDir,
      ...(mode === "record" ? { upstreams: [{ logicalOrigin: definition.logicalOrigin ?? FIXTURE_MODEL_ORIGIN, transportOrigin: upstream.url }] } : {}),
      provenance: { ...manifest.provenance, scenario: definition.id },
    });
    report.timings.proxyBootMs = performance.now() - proxyBootStarted;
    const requests: ReplayRequestObservation[] = [];
    const context: ReplayScenarioContext = {
      world, sessionIds, identity, proxy, run: undefined, mode, runId: currentRun, upstream, bindings, requests,
      bind: (input) => bindings.declareAndBind({ owner: "world", ...input }),
      observe: (name, value) => { report.observations.push({ name, value, atMs: performance.now() - started }); },
      checked: (predicate) => (value) => {
        const errors = proxy.snapshot(currentRun).errors;
        if (errors.length) throw new Error(`proxy rejected ${definition.id}: ${JSON.stringify(errors)}`);
        return predicate(value);
      },
      waitForCalls: (predicate, timeoutMs = 30_000) => proxy.waitFor(currentRun, context.checked(predicate), { timeout: timeoutMs }),
      startTui: async (alias, options = {}) => {
        const sessionId = sessionIds[alias] ?? (identity.ownerBindings().find((owner) => owner.canonical === alias)?.live);
        if (!sessionId) throw new Error(`unknown session alias ${alias}`);
        const tui = await ReplayTui.start(world, { label: `${alias}-${terminals.length + 1}`, ...options, sessionId, proxy: proxy.url, run: currentRun! });
        terminals.push(tui);
        return tui;
      },
    };
    const supplied = await definition.runOptions?.(context) ?? {};
    const requestEvidence = syntheticRequestEvidence(world, mode);
    context.run = proxy.createRun(currentRun, {
      ...supplied,
      resolveContext: supplied.resolveContext ?? identity.resolve,
      bindings: bindings.registry,
      bindingOwnerFor: bindings.bindingOwnerFor,
      requestRules: (call: any) => [...bindings.requestRules(call), ...(typeof supplied.requestRules === "function" ? supplied.requestRules(call) : supplied.requestRules ?? [])],
      portableResponses: mode === "replay" ? manifest.portableResponses : undefined,
      onRequest: async (observation: ReplayRequestObservation) => {
        syncOwners();
        await supplied.onRequest?.(observation);
        await bindings.onRequest(observation);
        requests.push({ ...observation, request: { ...observation.request, body: Buffer.from(observation.request.body) } });
        await requestEvidence(observation);
      },
    });
    report.timings.preparationMs = performance.now() - started - report.timings.proxyBootMs;
    scenarioStarted = performance.now();
    await definition.run(context);
    report.timings.driverMs = performance.now() - scenarioStarted;
    driverFinished = true;
    await context.waitForCalls((snapshot) => snapshot.active === 0);
    if (mode === "record" && Array.isArray(upstream.errors) && upstream.errors.length) throw new Error(`fixture upstream errors: ${JSON.stringify(upstream.errors)}`);
    for (const tui of terminals) { await tui.save("final"); await tui.stop(); }
    await cp(world.work, path.join(world.evidence, "final-work"), { recursive: true, dereference: false });
    report.proxy = await proxy.completeRun(currentRun);
    if (mode === "replay" && report.proxy.upstreamCalls !== 0) throw new Error("offline replay contacted an upstream");
    if (mode === "record") {
      await proxy.seal();
      manifest.bindings = bindings.snapshotDeclarations();
      manifest.portableResponses = await bindings.responsePlans(cassetteDir, { tools: definition.portableTools });
      await writeFile(path.join(staging!, "scenario.json"), JSON.stringify(manifest, null, 2));
      const previous = `${fixtureDirectory}.previous-${randomUUID()}`;
      let hadPrevious = false;
      try { await rename(fixtureDirectory, previous); hadPrevious = true; }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
      try { await rename(staging!, fixtureDirectory); staging = undefined; }
      catch (error) { if (hadPrevious) await rename(previous, fixtureDirectory); throw error; }
      if (hadPrevious) await rm(previous, { recursive: true, force: true });
    }
    report.status = "passed";
    return report;
  } catch (error) {
    primaryFailure = true;
    const failure = error instanceof Error ? error : new Error(String(error));
    report.error = { name: failure.name, message: failure.message, stack: failure.stack };
    throw error;
  } finally {
    const cleanupStarted = performance.now();
    if (scenarioStarted === undefined) report.timings.preparationMs = cleanupStarted - started - report.timings.proxyBootMs;
    else if (!driverFinished) report.timings.driverMs = cleanupStarted - scenarioStarted;
    const cleanupErrors: Error[] = [];
    const cleanup = async (work: () => Promise<unknown> | unknown) => {
      try { await work(); } catch (error) { cleanupErrors.push(error instanceof Error ? error : new Error(String(error))); }
    };
    for (const tui of terminals) {
      if (report.status !== "passed") await cleanup(() => tui.save("failure"));
      await cleanup(() => tui.stop());
    }
    if (proxy && currentRun && !report.proxy) {
      try { report.proxy = proxy.snapshot(currentRun); } catch { /* Failure can precede run registration. */ }
    }
    await cleanup(() => proxy?.close());
    await cleanup(() => upstream?.close());
    if (report.status === "passed" && mode === "record") {
      // A replay must fail if it accidentally depends on a recording directory instead of its fresh seed.
      await cleanup(() => Promise.all([rm(world.home, { recursive: true, force: true }), rm(world.work, { recursive: true, force: true })]));
    }
    if (staging) await cleanup(() => rm(staging!, { recursive: true, force: true }));
    if (lock) await cleanup(async () => {
      const owner = JSON.parse(await readFile(path.join(lock!, "owner.json"), "utf8"));
      if (owner.token !== lockToken) throw new Error("recording lock ownership changed during this run");
      await rm(lock!, { recursive: true, force: true });
    });
    if (cleanupErrors.length) {
      report.status = "failed";
      report.observations.push({ name: "cleanup-errors", value: cleanupErrors.map((error) => error.message), atMs: performance.now() - started });
      report.error ??= { name: cleanupErrors[0]!.name, message: cleanupErrors[0]!.message, stack: cleanupErrors[0]!.stack };
    }
    report.timings.scenarioMs = scenarioStarted === undefined ? 0 : cleanupStarted - scenarioStarted;
    report.timings.cleanupMs = performance.now() - cleanupStarted;
    report.timings.totalMs = performance.now() - started;
    await writeFile(path.join(world.evidence, "report.json"), JSON.stringify(report, null, 2));
    console.log(`REPLAY_CASE ${definition.id} ${mode} ${report.status} prep=${report.timings.preparationMs.toFixed(0)}ms run=${report.timings.scenarioMs.toFixed(0)}ms evidence=${world.evidence}`);
    if (!primaryFailure && cleanupErrors.length) throw new AggregateError(cleanupErrors, "scenario cleanup failed");
  }
}
