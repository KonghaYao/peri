#!/usr/bin/env node

// src/cli.ts
import { readFileSync as readFileSync2 } from "node:fs";

// src/adlc.ts
import { createHash } from "node:crypto";
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readSync,
  realpathSync,
  statSync
} from "node:fs";
import { isAbsolute, join, relative, sep } from "node:path";
var ADLC_SCHEMA_VERSION = 1;
var MAX_ADLC_REQUEST_BYTES = 1024 * 1024;
var MAX_ADLC_ARTIFACTS = 256;
var MAX_ADLC_ARTIFACT_BYTES = 16 * 1024 * 1024;
var MAX_ADLC_TOTAL_ARTIFACT_BYTES = 64 * 1024 * 1024;

class AdlcProtocolError extends Error {
}
function fail(message) {
  throw new AdlcProtocolError(message);
}
function boundedString(value, name, max = 1024) {
  if (typeof value !== "string" || value.length === 0 || value.length > max) {
    fail(`${name} must be a non-empty string of at most ${max} bytes`);
  }
  return value;
}
function ensureSchema(value) {
  if (value !== undefined && value !== ADLC_SCHEMA_VERSION)
    fail("unsupported ADLC schemaVersion");
}
function normalizeSha(value, name) {
  const raw = boundedString(value, name, 71).toLowerCase();
  const hex = raw.startsWith("sha256:") ? raw.slice("sha256:".length) : raw;
  if (!/^[0-9a-f]{64}$/.test(hex))
    fail(`${name} must be sha256:<64 lowercase hex>`);
  return `sha256:${hex}`;
}
function agentId(value, name) {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    fail(`${name} must be a non-negative safe integer`);
  }
  return value;
}
function normalizeRelativePath(value, name) {
  const raw = boundedString(value, name, 512).replaceAll("\\", "/");
  if (isAbsolute(raw))
    fail(`${name} must be relative to canonicalRoot`);
  const parts = raw.split("/");
  if (parts.length === 0 || parts.some((part) => part === "" || part === "." || part === "..")) {
    fail(`${name} contains unsafe path components`);
  }
  return parts.join("/");
}
function within(root, candidate) {
  const child = relative(root, candidate);
  return child === "" || !child.startsWith(".." + sep) && child !== ".." && !isAbsolute(child);
}
function validateArtifactRequirements(value) {
  if (!Array.isArray(value))
    fail("requiredOutputs must be an array");
  if (value.length > MAX_ADLC_ARTIFACTS)
    fail(`requiredOutputs exceeds ${MAX_ADLC_ARTIFACTS} artifacts`);
  const seen = new Set;
  return value.map((raw, index) => {
    if (!raw || typeof raw !== "object")
      fail(`requiredOutputs[${index}] must be an object`);
    const item = raw;
    const path = normalizeRelativePath(item.path, `requiredOutputs[${index}].path`);
    if (seen.has(path))
      fail(`requiredOutputs contains duplicate path: ${path}`);
    seen.add(path);
    return {
      path,
      sha256: normalizeSha(item.sha256, `requiredOutputs[${index}].sha256`),
      ...item.label === undefined ? {} : { label: boundedString(item.label, `requiredOutputs[${index}].label`, 256) }
    };
  });
}
function addBlocker(blockers, kind, code, message, extra = {}) {
  blockers.push({ kind, code, message, ...extra });
}
function checkPathNoSymlink(root, path) {
  let cursor = root;
  for (const part of path.split("/")) {
    cursor = join(cursor, part);
    const info = lstatSync(cursor);
    if (info.isSymbolicLink())
      fail(`required output is a symlink: ${path}`);
  }
  const canonical = realpathSync(cursor);
  if (!within(root, canonical))
    fail(`required output escapes canonicalRoot: ${path}`);
  return canonical;
}
function readStableRegularFile(path, maxBytes) {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const before = fstatSync(fd);
    if (!before.isFile())
      throw new Error("not a regular file");
    const bytes = Number(before.size);
    if (!Number.isSafeInteger(bytes) || bytes <= 0)
      throw new Error("file is empty or too large");
    if (bytes > maxBytes)
      throw new Error(`file exceeds ${maxBytes} bytes`);
    const content = Buffer.alloc(bytes);
    let offset = 0;
    while (offset < bytes) {
      const read = readSync(fd, content, offset, bytes - offset, null);
      if (read === 0)
        throw new Error("file truncated while being read");
      offset += read;
    }
    const after = fstatSync(fd);
    if (before.dev !== after.dev || before.ino !== after.ino || before.size !== after.size || before.mtimeMs !== after.mtimeMs) {
      throw new Error("file changed while being read");
    }
    return { bytes, content };
  } finally {
    closeSync(fd);
  }
}
function validateStageRequest(request) {
  if (!request || typeof request !== "object")
    fail("ADLC stage request must be an object");
  ensureSchema(request.schemaVersion);
  if (typeof request.adlcDeclared !== "boolean")
    fail("adlcDeclared must be boolean");
  boundedString(request.stage, "stage", 128);
  const currentRunId = boundedString(request.currentRunId, "currentRunId", 256);
  if (!currentRunId)
    fail("currentRunId is required");
  if (typeof request.canonicalRoot !== "string" || !isAbsolute(request.canonicalRoot)) {
    fail("canonicalRoot must be an absolute path");
  }
  const root = realpathSync(request.canonicalRoot);
  if (!statSync(root).isDirectory())
    fail("canonicalRoot must be a directory");
  const outputs = validateArtifactRequirements(request.requiredOutputs);
  if (request.participantIdentities !== undefined && !Array.isArray(request.participantIdentities)) {
    fail("participantIdentities must be an array");
  }
  for (const [index, participant] of (request.participantIdentities ?? []).entries()) {
    if (!participant || typeof participant !== "object")
      fail(`participantIdentities[${index}] must be an object`);
    boundedString(participant.runId, `participantIdentities[${index}].runId`, 256);
    agentId(participant.agentId, `participantIdentities[${index}].agentId`);
  }
  if (request.producerIdentity !== undefined) {
    const producer = request.producerIdentity;
    if (!producer || typeof producer !== "object")
      fail("producerIdentity must be an object");
    boundedString(producer.runId, "producerIdentity.runId", 256);
    agentId(producer.agentId, "producerIdentity.agentId");
  }
  return { root, outputs };
}
function checkAdlcStage(request) {
  const { root, outputs } = validateStageRequest(request);
  const blockers = [];
  const artifacts = [];
  const pending = [];
  let totalBytes = 0;
  for (let index = 0;index < outputs.length; index += 1) {
    const output = outputs[index];
    const remainingBytes = MAX_ADLC_TOTAL_ARTIFACT_BYTES - totalBytes;
    if (remainingBytes <= 0) {
      addBlocker(blockers, "protocol", "required_output_budget_exceeded", `artifact byte budget exhausted before reading: ${output.path}`, { path: output.path });
      pending.push(...outputs.slice(index).map((item) => item.path));
      break;
    }
    try {
      const path = checkPathNoSymlink(root, output.path);
      const file = readStableRegularFile(path, Math.min(MAX_ADLC_ARTIFACT_BYTES, remainingBytes));
      totalBytes += file.bytes;
      const actualSha256 = `sha256:${createHash("sha256").update(file.content).digest("hex")}`;
      const valid = actualSha256 === output.sha256;
      artifacts.push({ path: output.path, expectedSha256: output.sha256, actualSha256, bytes: file.bytes, valid });
      if (!valid) {
        pending.push(output.path);
        addBlocker(blockers, "protocol", "required_output_stale", `required output hash does not match: ${output.path}`, { path: output.path });
      }
    } catch (error) {
      pending.push(output.path);
      const message = error.message;
      const budgetExceeded = message.includes("exceeds");
      addBlocker(blockers, "protocol", budgetExceeded ? "required_output_budget_exceeded" : "required_output_invalid", `${output.path}: ${message}`, { path: output.path });
      artifacts.push({ path: output.path, expectedSha256: output.sha256, valid: false });
      pending.push(...outputs.slice(index + 1).map((item) => item.path));
      break;
    }
  }
  if (request.stage === "assessment" && request.adlcDeclared) {
    const producer = request.producerIdentity;
    if (!producer) {
      addBlocker(blockers, "protocol", "assessment_producer_missing", "assessment requires host-observed producer identity");
    } else if (producer.runId !== request.currentRunId) {
      addBlocker(blockers, "protocol", "assessment_producer_run_mismatch", "assessment producer is from another run");
    } else if (!request.participantIdentities) {
      addBlocker(blockers, "protocol", "assessment_participants_missing", "assessment participant identities are required");
    } else if (request.participantIdentities.some((participant) => participant.runId === producer.runId && participant.agentId === producer.agentId)) {
      addBlocker(blockers, "protocol", "assessment_not_independent", "assessment producer is a participant in this run");
    }
  }
  const reusable = artifacts.filter((artifact) => artifact.valid).map((artifact) => artifact.path);
  if (request.adlcDeclared && outputs.length === 0) {
    addBlocker(blockers, "protocol", "required_outputs_empty", "ADLC stage has no declared required outputs");
  }
  const actions = blockers.map((blocker) => ({
    kind: blocker.code.startsWith("assessment_") ? "assess" : "repair",
    reason: blocker.message,
    ...blocker.path ? { packageId: blocker.path } : {}
  }));
  const ready = blockers.length === 0;
  return {
    schema: "peri.adlc/stage-result-v1",
    ok: ready,
    ready,
    adlcDeclared: request.adlcDeclared,
    stage: request.stage,
    semanticAcceptance: "not_checked",
    reusable,
    pending,
    blockers,
    actions,
    artifacts
  };
}
function snapshot(value, name) {
  if (!value || typeof value !== "object")
    fail(`${name} must be an object`);
  const raw = value;
  const dependencies = raw.dependencies;
  if (!dependencies || typeof dependencies !== "object" || Array.isArray(dependencies))
    fail(`${name}.dependencies must be an object`);
  const verifiedArtifacts = raw.verifiedArtifacts;
  if (!Array.isArray(verifiedArtifacts) || verifiedArtifacts.length > MAX_ADLC_ARTIFACTS)
    fail(`${name}.verifiedArtifacts is invalid or too large`);
  const normalizedArtifacts = verifiedArtifacts.map((item, index) => {
    if (!item || typeof item !== "object")
      fail(`${name}.verifiedArtifacts[${index}] must be an object`);
    const record = item;
    return {
      path: normalizeRelativePath(record.path, `${name}.verifiedArtifacts[${index}].path`),
      sha256: normalizeSha(record.sha256, `${name}.verifiedArtifacts[${index}].sha256`)
    };
  }).sort((a, b) => a.path.localeCompare(b.path));
  return {
    contract: boundedString(raw.contract, `${name}.contract`, 512),
    input: boundedString(raw.input, `${name}.input`, 512),
    dependencies: Object.fromEntries(Object.entries(dependencies).sort(([a], [b]) => a.localeCompare(b)).map(([key, value2]) => [boundedString(key, `${name}.dependencies key`, 256), boundedString(value2, `${name}.dependencies.${key}`, 512)])),
    verifiedArtifacts: normalizedArtifacts
  };
}
function snapshotsEqual(a, b) {
  return a.contract === b.contract && a.input === b.input && JSON.stringify(a.dependencies) === JSON.stringify(b.dependencies) && JSON.stringify(a.verifiedArtifacts) === JSON.stringify(b.verifiedArtifacts);
}
function validatePlanRequest(request) {
  if (!request || typeof request !== "object")
    fail("ADLC plan request must be an object");
  ensureSchema(request.schemaVersion);
  if (!Array.isArray(request.packages) || request.packages.length === 0 || request.packages.length > MAX_ADLC_ARTIFACTS) {
    fail(`packages must contain 1-${MAX_ADLC_ARTIFACTS} entries`);
  }
  const ids = new Set;
  const packages = request.packages.map((raw, index) => {
    if (!raw || typeof raw !== "object")
      fail(`packages[${index}] must be an object`);
    const item = raw;
    const id = boundedString(item.id, `packages[${index}].id`, 256);
    if (ids.has(id))
      fail(`duplicate package id: ${id}`);
    ids.add(id);
    const status = item.status;
    if (!["complete", "incomplete", "blocked", "pending", "invalid"].includes(status))
      fail(`packages[${index}].status is invalid`);
    if (typeof item.requiredChecksPassed !== "boolean")
      fail(`packages[${index}].requiredChecksPassed must be boolean`);
    if (item.sourceCompileGatePassed !== undefined && typeof item.sourceCompileGatePassed !== "boolean")
      fail(`packages[${index}].sourceCompileGatePassed must be boolean`);
    if (item.assessment !== undefined && typeof item.assessment !== "boolean")
      fail(`packages[${index}].assessment must be boolean`);
    const dependencies = item.dependencies ?? [];
    if (!Array.isArray(dependencies) || dependencies.some((dependency) => typeof dependency !== "string"))
      fail(`packages[${index}].dependencies must be string[]`);
    return {
      ...item,
      id,
      dependencies: [...new Set(dependencies)].sort(),
      current: snapshot(item.current, `packages[${index}].current`),
      ...item.previous === undefined ? {} : { previous: snapshot(item.previous, `packages[${index}].previous`) }
    };
  });
  for (const item of packages) {
    for (const dependency of item.dependencies ?? [])
      if (!ids.has(dependency))
        fail(`package ${item.id} references unknown dependency ${dependency}`);
  }
  return packages;
}
function planAdlcRecovery(request) {
  const packages = validatePlanRequest(request);
  const inputBlockers = request.blockers ?? [];
  if (!Array.isArray(inputBlockers))
    fail("blockers must be an array");
  const blockers = inputBlockers.map((blocker, index) => {
    if (!blocker || typeof blocker !== "object")
      fail(`blockers[${index}] must be an object`);
    if (!["product_gap", "capability", "protocol", "closeout"].includes(blocker.kind))
      fail(`blockers[${index}].kind is invalid`);
    return {
      kind: blocker.kind,
      code: boundedString(blocker.code, `blockers[${index}].code`, 256),
      message: boundedString(blocker.message, `blockers[${index}].message`, 2048),
      ...blocker.packageId === undefined ? {} : { packageId: boundedString(blocker.packageId, `blockers[${index}].packageId`, 256) },
      ...blocker.path === undefined ? {} : { path: boundedString(blocker.path, `blockers[${index}].path`, 512) }
    };
  });
  const byId = new Map(packages.map((item) => [item.id, item]));
  if (request.invalidPackageIds !== undefined && !Array.isArray(request.invalidPackageIds))
    fail("invalidPackageIds must be an array");
  const invalidated = new Set((request.invalidPackageIds ?? []).map((id) => boundedString(id, "invalidPackageIds[]", 256)));
  for (const id of invalidated)
    if (!byId.has(id))
      fail(`invalidPackageIds references unknown package ${id}`);
  const blockedByObservation = new Set;
  for (const blocker of blockers) {
    if (blocker.packageId !== undefined) {
      if (!byId.has(blocker.packageId))
        fail(`blocker references unknown package ${blocker.packageId}`);
      blockedByObservation.add(blocker.packageId);
    }
  }
  for (const item of packages)
    if (item.status === "blocked")
      blockedByObservation.add(item.id);
  const blockedClosure = new Set(blockedByObservation);
  let blockedChanged = true;
  while (blockedChanged) {
    blockedChanged = false;
    for (const item of packages) {
      if (item.dependencies?.some((dependency) => blockedClosure.has(dependency)) && !blockedClosure.has(item.id)) {
        blockedClosure.add(item.id);
        blockedChanged = true;
      }
    }
  }
  const visit = new Set;
  const visited = new Set;
  const visitPackage = (id) => {
    if (visit.has(id))
      fail(`package dependency cycle includes ${id}`);
    if (visited.has(id))
      return;
    visit.add(id);
    for (const dependency of byId.get(id).dependencies ?? [])
      visitPackage(dependency);
    visit.delete(id);
    visited.add(id);
  };
  for (const item of packages)
    visitPackage(item.id);
  for (const item of packages) {
    if (item.status === "invalid" || item.previous !== undefined && !snapshotsEqual(item.current, item.previous))
      invalidated.add(item.id);
    if (item.sourceCompileGatePassed === false)
      invalidated.add(item.id);
  }
  let changed = true;
  while (changed) {
    changed = false;
    for (const item of packages) {
      if (item.dependencies?.some((dependency) => invalidated.has(dependency)) && !invalidated.has(item.id)) {
        invalidated.add(item.id);
        changed = true;
      }
    }
  }
  const validated = new Set;
  let validatedChanged = true;
  while (validatedChanged) {
    validatedChanged = false;
    for (const item of packages) {
      const ownValid = item.status === "complete" && item.requiredChecksPassed && item.sourceCompileGatePassed !== false && !invalidated.has(item.id) && !blockedClosure.has(item.id);
      const dependenciesValid = (item.dependencies ?? []).every((dependency) => validated.has(dependency));
      if (ownValid && dependenciesValid && !validated.has(item.id)) {
        validated.add(item.id);
        validatedChanged = true;
      }
    }
  }
  const reusable = new Set;
  let reusableChanged = true;
  while (reusableChanged) {
    reusableChanged = false;
    for (const item of packages) {
      const ownEvidenceUnchanged = validated.has(item.id) && item.previous !== undefined && snapshotsEqual(item.current, item.previous);
      const dependenciesReusable = (item.dependencies ?? []).every((dependency) => reusable.has(dependency));
      if (ownEvidenceUnchanged && dependenciesReusable && !reusable.has(item.id)) {
        reusable.add(item.id);
        reusableChanged = true;
      }
    }
  }
  const globalBlocker = blockers.some((blocker) => blocker.packageId === undefined);
  const ready = new Set;
  if (!globalBlocker) {
    for (const item of packages) {
      const dependenciesValid = (item.dependencies ?? []).every((dependency) => validated.has(dependency));
      if (!validated.has(item.id) && !blockedClosure.has(item.id) && dependenciesValid)
        ready.add(item.id);
    }
  }
  const pending = packages.filter((item) => !validated.has(item.id)).map((item) => item.id).sort();
  const packagePlans = packages.map((item) => ({
    id: item.id,
    status: item.status,
    dependencies: item.dependencies ?? [],
    invalidated: invalidated.has(item.id),
    validated: validated.has(item.id),
    reusable: reusable.has(item.id),
    ready: ready.has(item.id),
    fingerprintsMatch: item.previous !== undefined && snapshotsEqual(item.current, item.previous),
    verifiedArtifactCount: item.current.verifiedArtifacts.length,
    ...!validated.has(item.id) ? { reason: globalBlocker ? "global_blocker" : blockedClosure.has(item.id) ? "observed_blocker" : invalidated.has(item.id) ? "fingerprint_or_dependency_changed" : item.sourceCompileGatePassed === false ? "source_compile_gate_failed" : "required_checks_pending" } : {}
  }));
  const actions = [];
  for (const blocker of blockers) {
    actions.push({ kind: blocker.kind === "capability" ? "diagnose" : blocker.kind === "closeout" ? "closeout" : "repair", reason: blocker.message, ...blocker.packageId ? { packageId: blocker.packageId } : {} });
  }
  const assessmentOnly = blockers.length === 0 && packages.filter((item) => item.assessment && ready.has(item.id)).length === 1 && packages.filter((item) => !item.assessment && !validated.has(item.id)).length === 0;
  if (assessmentOnly) {
    const assessor = packages.find((item) => item.assessment && ready.has(item.id));
    actions.push({ kind: "assess", packageId: assessor.id, reason: "independent assessment is the sole missing result" });
  } else {
    for (const item of packages) {
      if (!ready.has(item.id) || item.assessment)
        continue;
      const shouldRerun = invalidated.has(item.id) || item.sourceCompileGatePassed === false || item.status === "complete";
      actions.push({ kind: shouldRerun ? "rerun" : "start", packageId: item.id, reason: shouldRerun ? item.sourceCompileGatePassed === false ? "source compile gate failed" : "package or dependency fingerprint changed" : "package checkpoint is pending" });
    }
  }
  return {
    schema: "peri.adlc/plan-v1",
    ok: true,
    planValid: true,
    semanticAcceptance: "not_checked",
    ready: [...ready].sort(),
    reusable: [...reusable].sort(),
    pending,
    blockers,
    actions,
    packages: packagePlans
  };
}
function readAdlcJson(path) {
  const info = lstatSync(path);
  if (!info.isFile() || info.isSymbolicLink())
    fail("ADLC request must be a regular non-symlink file");
  if (info.size > MAX_ADLC_REQUEST_BYTES)
    fail(`ADLC request exceeds ${MAX_ADLC_REQUEST_BYTES} bytes`);
  return JSON.parse(readStableRegularFile(path, MAX_ADLC_REQUEST_BYTES).content.toString("utf8"));
}
function adlcFile(args) {
  const operation = args[0];
  const requestPath = args[1];
  if (operation !== "check-stage" && operation !== "plan" || args.length !== 2 || !requestPath || requestPath.startsWith("--")) {
    console.error("用法：peri-workflow adlc check-stage|plan <request.json>");
    process.exit(1);
  }
  try {
    const request = readAdlcJson(requestPath);
    const result = operation === "check-stage" ? checkAdlcStage(request) : planAdlcRecovery(request);
    console.log(JSON.stringify(result, null, 2));
    if (!result.ok)
      process.exit(2);
  } catch (error) {
    console.error(`adlc ${operation} failed: ${error.message}`);
    process.exit(1);
  }
}

// src/boundary.ts
import { createHash as createHash2 } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  closeSync as closeSync2,
  constants as constants2,
  fstatSync as fstatSync2,
  lstatSync as lstatSync2,
  opendirSync,
  openSync as openSync2,
  readlinkSync,
  readSync as readSync2,
  realpathSync as realpathSync2,
  statSync as statSync2,
  writeFileSync
} from "node:fs";
import { dirname, isAbsolute as isAbsolute2, join as join2, relative as relative2, sep as sep2 } from "node:path";
import { performance } from "node:perf_hooks";
var BOUNDARY_SCHEMA_VERSION = 1;
var BOUNDARY_SCANNER_VERSION = "adlc-boundary-v1";
var MAX_BOUNDARY_REQUEST_BYTES = 1024 * 1024;
var MAX_BOUNDARY_BASELINE_BYTES = 64 * 1024 * 1024;
var MAX_BOUNDARY_ENTRIES = 1e5;
var MAX_BOUNDARY_BYTES = 1073741824;
var MAX_BOUNDARY_DEPTH = 128;
var MAX_BOUNDARY_DEADLINE_MS = 60000;
var MAX_GIT_OUTPUT_BYTES = 1024 * 1024;
var IO_CHUNK_BYTES = 1024 * 1024;
var MAX_BOUNDARY_ROOT_DECLARATIONS = 1024;

class BoundaryError extends Error {
}
function fail2(message) {
  throw new BoundaryError(message);
}
function positiveInteger(value, name) {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    fail2(`${name} must be a positive safe integer`);
  }
  return value;
}
function boundedInteger(value, name, maximum) {
  const result = positiveInteger(value, name);
  if (result > maximum)
    fail2(`${name} exceeds hard maximum ${maximum}`);
  return result;
}
function rootPath(value) {
  const path = typeof value === "string" ? value : value.path;
  if (typeof path !== "string" || path.length === 0)
    fail2("boundary root path must be non-empty");
  return path;
}
function validateRelative(value, field) {
  if (isAbsolute2(value))
    fail2(`${field} must be repository-relative`);
  const normalized = value.replaceAll("\\", "/");
  if (normalized === "" || normalized === ".")
    return ".";
  const parts = normalized.split("/");
  if (parts.some((part) => part === ".." || part === ""))
    fail2(`${field} contains unsafe path components`);
  return parts.filter((part) => part !== ".").join("/");
}
function isWithin(root, candidate) {
  const child = relative2(root, candidate);
  return child === "" || !child.startsWith(".." + sep2) && child !== ".." && !isAbsolute2(child);
}
function identity(path) {
  return identityFromStat(lstatSync2(path));
}
function identityFromStat(meta) {
  return {
    dev: Number(meta.dev ?? 0),
    ino: Number(meta.ino ?? 0),
    birthtimeMs: Math.trunc(meta.birthtimeMs || 0)
  };
}
function version(meta) {
  return {
    ...identityFromStat(meta),
    size: meta.size,
    mtimeMs: meta.mtimeMs,
    ctimeMs: meta.ctimeMs,
    isFile: meta.isFile(),
    isDirectory: meta.isDirectory()
  };
}
function sameVersion(a, b) {
  return sameIdentity(a, b) && a.size === b.size && a.mtimeMs === b.mtimeMs && a.ctimeMs === b.ctimeMs && a.isFile === b.isFile && a.isDirectory === b.isDirectory;
}
function lstatIfPresent(path) {
  try {
    return lstatSync2(path);
  } catch (error) {
    const code = error.code;
    if (code === "ENOENT" || code === "ENOTDIR")
      return;
    throw error;
  }
}
function openReadOnlyNoFollow(path) {
  return openSync2(path, constants2.O_RDONLY | constants2.O_NOFOLLOW | constants2.O_NONBLOCK);
}
function readBoundedUtf8(path, maximum, label) {
  const before = lstatIfPresent(path);
  if (!before || before.isSymbolicLink() || !before.isFile())
    fail2(`${label} must be a regular file`);
  if (before.size > maximum)
    fail2(`${label} exceeds ${maximum} bytes`);
  const beforeVersion = version(before);
  const canonicalBefore = realpathSync2(path);
  const fd = openReadOnlyNoFollow(path);
  const chunks = [];
  let total = 0;
  try {
    const opened = fstatSync2(fd);
    if (!opened.isFile() || !sameVersion(beforeVersion, version(opened)))
      fail2(`${label} race detected`);
    const buffer = Buffer.allocUnsafe(Math.min(IO_CHUNK_BYTES, Math.max(1, before.size)));
    while (total < before.size) {
      const count = readSync2(fd, buffer, 0, Math.min(buffer.length, before.size - total), total);
      if (count === 0)
        fail2(`${label} changed while reading`);
      chunks.push(Buffer.from(buffer.subarray(0, count)));
      total += count;
      if (total > maximum)
        fail2(`${label} exceeds ${maximum} bytes`);
    }
    const after = fstatSync2(fd);
    const canonicalAfter = realpathSync2(path);
    if (!after.isFile() || total !== after.size || !sameVersion(beforeVersion, version(after)) || canonicalAfter !== canonicalBefore) {
      fail2(`${label} race detected`);
    }
  } finally {
    closeSync2(fd);
  }
  return Buffer.concat(chunks, total).toString("utf8");
}
function sameIdentity(a, b) {
  return !!a && !!b && a.dev === b.dev && a.ino === b.ino && a.birthtimeMs === b.birthtimeMs;
}
function canonicalRepoRoot(raw) {
  if (typeof raw !== "string" || raw.length === 0 || !isAbsolute2(raw)) {
    fail2("repoRoot must be an existing absolute path");
  }
  const root = realpathSync2(raw);
  if (!statSync2(root).isDirectory())
    fail2("repoRoot must be a directory");
  return root;
}
function resolveExistingOrParent(repoRoot, raw, field) {
  const rel = validateRelative(raw, field);
  const candidate = join2(repoRoot, rel);
  const existing = lstatIfPresent(candidate);
  const parent = existing ? candidate : dirname(candidate);
  const canonicalParent = realpathSync2(parent);
  if (!isWithin(repoRoot, canonicalParent)) {
    fail2(`${field} escapes repoRoot through a symlink`);
  }
  if (!existing)
    return { path: candidate, relative: rel, exists: false };
  const canonical = realpathSync2(candidate);
  if (!isWithin(repoRoot, canonical)) {
    fail2(`${field} escapes repoRoot through a symlink`);
  }
  return { path: canonical, relative: rel, exists: true };
}
function normalizeRequest(request) {
  if (!request || request.schemaVersion !== BOUNDARY_SCHEMA_VERSION)
    fail2("unsupported boundary schemaVersion");
  if (request.operation !== "snapshot" && request.operation !== "compare")
    fail2("operation must be snapshot or compare");
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(request.baselineId)) {
    fail2("baselineId must be a lowercase UUID");
  }
  if (request.expectedBaselineFingerprint !== undefined && !/^sha256:[0-9a-f]{64}$/.test(request.expectedBaselineFingerprint))
    fail2("expectedBaselineFingerprint must be sha256:<64 lowercase hex>");
  const repoRoot = canonicalRepoRoot(request.repoRoot);
  const cwd = request.cwd ? realpathSync2(request.cwd) : repoRoot;
  if (!isWithin(repoRoot, cwd))
    fail2("cwd escapes repoRoot");
  const limits = request.limits;
  if (!limits)
    fail2("limits are required");
  const normalizedLimits = {
    maxEntries: boundedInteger(limits.maxEntries, "limits.maxEntries", MAX_BOUNDARY_ENTRIES),
    maxBytes: boundedInteger(limits.maxBytes, "limits.maxBytes", MAX_BOUNDARY_BYTES),
    maxDepth: boundedInteger(limits.maxDepth, "limits.maxDepth", MAX_BOUNDARY_DEPTH),
    deadlineMs: boundedInteger(limits.deadlineMs, "limits.deadlineMs", MAX_BOUNDARY_DEADLINE_MS)
  };
  for (const [field, value] of [
    ["observedRoots", request.observedRoots],
    ["generatedRoots", request.generatedRoots],
    ["skipRoots", request.skipRoots],
    ["pathAllowlist", request.pathAllowlist]
  ]) {
    if (value !== undefined && (!Array.isArray(value) || value.length > MAX_BOUNDARY_ROOT_DECLARATIONS)) {
      fail2(`${field} exceeds hard maximum ${MAX_BOUNDARY_ROOT_DECLARATIONS}`);
    }
  }
  const roots = (request.observedRoots ?? ["."]).map((root) => resolveExistingOrParent(repoRoot, rootPath(root), "observedRoots").relative);
  const generatedRoots = (request.generatedRoots ?? []).map((root) => resolveExistingOrParent(repoRoot, rootPath(root), "generatedRoots").relative);
  if (generatedRoots.some((root) => root === "."))
    fail2("generatedRoots cannot authorize repoRoot");
  const skipRoots = (request.skipRoots ?? [".git"]).map((root) => resolveExistingOrParent(repoRoot, rootPath(root), "skipRoots").relative);
  const pathAllowlist = (request.pathAllowlist ?? []).map((path) => validateRelative(path, "pathAllowlist"));
  for (const generated of generatedRoots) {
    if (!isAllowed(generated, pathAllowlist))
      fail2(`generated root is outside pathAllowlist: ${generated}`);
  }
  const baseline = resolveExistingOrParent(repoRoot, request.baselinePath, "baselinePath");
  if (baseline.exists && request.operation === "snapshot")
    fail2("baselinePath already exists");
  if (baseline.exists && lstatSync2(baseline.path).isSymbolicLink())
    fail2("baselinePath must not be a symlink");
  if (!isWithin(repoRoot, baseline.path) || baseline.relative === ".")
    fail2("baselinePath must be below repoRoot");
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
    scannerVersion: BOUNDARY_SCANNER_VERSION
  };
  const requestFingerprint = sha256(stableJson(identityInput));
  return { ...identityInput, requestFingerprint, expectedBaselineFingerprint: request.expectedBaselineFingerprint };
}
function stableJson(value) {
  return JSON.stringify(value);
}
function sha256(value) {
  return createHash2("sha256").update(value).digest("hex");
}
function isAllowed(path, allowlist) {
  return allowlist.some((allowed) => allowed === "." || path === allowed || path.startsWith(`${allowed}/`));
}
function targetOf(path) {
  return readlinkSync(path, "utf8");
}
function generatedRootHasTrackedFiles(repoRoot, path, deadlineAt) {
  const remainingMs = Math.floor(deadlineAt - performance.now());
  if (remainingMs <= 0)
    fail2("boundary scan deadline exceeded");
  const result = spawnSync("git", ["-C", repoRoot, "ls-files", "-z", "--", `:(literal)${path}`], {
    encoding: "buffer",
    maxBuffer: MAX_GIT_OUTPUT_BYTES,
    timeout: remainingMs
  });
  if (result.error || result.status !== 0 || result.stdout.length >= MAX_GIT_OUTPUT_BYTES) {
    fail2(`cannot establish tracked-file status for generated root: ${path}`);
  }
  return result.stdout.length > 0;
}
function generatedState(repoRoot, paths, deadlineAt) {
  return paths.map((path) => {
    const absolute = join2(repoRoot, path);
    const stat = lstatIfPresent(absolute);
    if (generatedRootHasTrackedFiles(repoRoot, path, deadlineAt))
      fail2(`generated root contains tracked files: ${path}`);
    if (!stat)
      return { path, exists: false, type: "absent" };
    if (!stat.isDirectory() || stat.isSymbolicLink())
      fail2(`generated root must be a real directory: ${path}`);
    return { path, exists: true, type: "directory", identity: identity(absolute) };
  });
}
function guardDirectory(repoRoot, absolute, path) {
  const before = lstatIfPresent(absolute);
  if (!before || before.isSymbolicLink() || !before.isDirectory())
    fail2(`directory changed while scanning: ${path}`);
  const canonical = realpathSync2(absolute);
  if (!isWithin(repoRoot, canonical))
    fail2(`directory escapes repoRoot while scanning: ${path}`);
  const fd = openReadOnlyNoFollow(absolute);
  let opened;
  try {
    opened = fstatSync2(fd);
  } finally {
    closeSync2(fd);
  }
  if (!opened.isDirectory() || !sameVersion(version(before), version(opened)))
    fail2(`directory race detected: ${path}`);
  return { canonical, version: version(before) };
}
function verifyDirectory(repoRoot, absolute, path, expected) {
  const after = lstatIfPresent(absolute);
  if (!after || after.isSymbolicLink() || !after.isDirectory())
    fail2(`directory race detected: ${path}`);
  const canonical = realpathSync2(absolute);
  if (canonical !== expected.canonical || !isWithin(repoRoot, canonical) || !sameVersion(expected.version, version(after))) {
    fail2(`directory race detected: ${path}`);
  }
}
function hashRegularFile(repoRoot, absolute, path, remainingBytes, checkBudget) {
  const before = lstatIfPresent(absolute);
  if (!before || before.isSymbolicLink() || !before.isFile())
    fail2(`file changed while scanning: ${path}`);
  const beforeVersion = version(before);
  if (before.size > remainingBytes)
    fail2("boundary scan maxBytes exceeded");
  const canonicalBefore = realpathSync2(absolute);
  if (!isWithin(repoRoot, canonicalBefore))
    fail2(`file escapes repoRoot while scanning: ${path}`);
  const fd = openReadOnlyNoFollow(absolute);
  const hash = createHash2("sha256");
  const buffer = Buffer.allocUnsafe(Math.min(IO_CHUNK_BYTES, Math.max(1, before.size)));
  let position = 0;
  try {
    const opened = fstatSync2(fd);
    if (!opened.isFile() || !sameVersion(beforeVersion, version(opened)))
      fail2(`file race detected: ${path}`);
    while (position < before.size) {
      checkBudget();
      const count = readSync2(fd, buffer, 0, Math.min(buffer.length, before.size - position), position);
      if (count === 0)
        fail2(`boundary file changed while reading: ${path}`);
      hash.update(buffer.subarray(0, count));
      position += count;
    }
    const after = fstatSync2(fd);
    const canonicalAfter = realpathSync2(absolute);
    if (!after.isFile() || !sameVersion(beforeVersion, version(after)) || canonicalAfter !== canonicalBefore || !isWithin(repoRoot, canonicalAfter)) {
      fail2(`file race detected: ${path}`);
    }
  } finally {
    closeSync2(fd);
  }
  return { size: before.size, digest: `sha256:${hash.digest("hex")}` };
}
function scan(request) {
  const started = performance.now();
  let readFiles = 0;
  let readBytes = 0;
  let entriesCount = 0;
  let enumeratedEntries = 0;
  const entries = [];
  const generatedRoots = generatedState(request.repoRoot, request.generatedRoots, started + request.limits.deadlineMs);
  const excludedRoots = [...new Set([...request.skipRoots, ...request.generatedRoots])].sort();
  const generatedSet = new Set(request.generatedRoots);
  const skipSet = new Set(request.skipRoots);
  const rootEntries = request.observedRoots;
  const fullScope = rootEntries.length === 1 && rootEntries[0] === ".";
  const errors = [];
  const checkBudget = () => {
    if (performance.now() - started > request.limits.deadlineMs)
      fail2("boundary scan deadline exceeded");
    if (entriesCount >= request.limits.maxEntries)
      fail2("boundary scan maxEntries exceeded");
  };
  const childrenOf = (absolute) => {
    const dir = opendirSync(absolute);
    const children = [];
    try {
      for (;; ) {
        if (performance.now() - started > request.limits.deadlineMs)
          fail2("boundary scan deadline exceeded");
        const entry = dir.readSync();
        if (!entry)
          break;
        enumeratedEntries += 1;
        if (enumeratedEntries > request.limits.maxEntries)
          fail2("boundary scan maxEntries exceeded");
        children.push(entry.name);
      }
    } finally {
      dir.closeSync();
    }
    return children.sort();
  };
  const visit = (absolute, path, depth) => {
    checkBudget();
    if (path === request.baselinePath)
      return;
    if (generatedSet.has(path))
      return;
    if (skipSet.has(path))
      return;
    if (depth > request.limits.maxDepth)
      fail2("boundary scan maxDepth exceeded");
    let stat;
    try {
      stat = lstatSync2(absolute);
    } catch (error) {
      fail2(`boundary entry cannot be read: ${path}: ${error.message}`);
    }
    entriesCount += 1;
    if (stat.isSymbolicLink()) {
      entries.push({ path, type: "symlink", size: stat.size, target: targetOf(absolute) });
      return;
    }
    if (stat.isDirectory()) {
      const guarded = guardDirectory(request.repoRoot, absolute, path);
      entries.push({ path, type: "directory", size: 0, identity: identityFromStat(stat) });
      const children = childrenOf(absolute);
      for (const child of children)
        visit(join2(absolute, child), `${path}/${child}`, depth + 1);
      verifyDirectory(request.repoRoot, absolute, path, guarded);
      return;
    }
    if (stat.isFile()) {
      const file = hashRegularFile(request.repoRoot, absolute, path, request.limits.maxBytes - readBytes, checkBudget);
      readFiles += 1;
      readBytes += file.size;
      entries.push({ path, type: "file", size: file.size, digest: file.digest });
      return;
    }
    entries.push({ path, type: "other", size: stat.size });
  };
  for (const root of rootEntries) {
    if (root === ".") {
      const guarded = guardDirectory(request.repoRoot, request.repoRoot, ".");
      const children = childrenOf(request.repoRoot);
      for (const child of children)
        visit(join2(request.repoRoot, child), child, 1);
      verifyDirectory(request.repoRoot, request.repoRoot, ".", guarded);
    } else if (generatedSet.has(root) || skipSet.has(root)) {
      continue;
    } else {
      const absolute = join2(request.repoRoot, root);
      if (!lstatIfPresent(absolute))
        continue;
      visit(absolute, root, 0);
    }
  }
  entries.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  const elapsedMs = Math.max(0, Math.round(performance.now() - started));
  return {
    entries,
    generatedRoots,
    coverage: {
      complete: fullScope && request.skipRoots.every((root) => root === ".git") && errors.length === 0,
      scope: fullScope ? "full" : "partial",
      readFiles,
      readBytes,
      elapsedMs,
      maxEntries: request.limits.maxEntries,
      maxBytes: request.limits.maxBytes,
      maxDepth: request.limits.maxDepth,
      deadlineMs: request.limits.deadlineMs,
      entries: entries.length,
      excludedRoots,
      errors
    }
  };
}
function snapshotDocument(request, scanResult) {
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
    coverage: scanResult.coverage
  };
  return { ...body, fingerprint: `sha256:${sha256(stableJson(body))}` };
}
function writeExclusive(path, content) {
  if (Buffer.byteLength(content, "utf8") > MAX_BOUNDARY_BASELINE_BYTES) {
    fail2(`boundary baseline exceeds ${MAX_BOUNDARY_BASELINE_BYTES} bytes`);
  }
  try {
    writeFileSync(path, content, { encoding: "utf8", flag: "wx" });
  } catch (error) {
    fail2(`cannot publish boundary baseline: ${error.message}`);
  }
}
function summaryFromCoverage(operation, request, coverage) {
  return {
    ok: coverage.complete,
    operation,
    baselineId: request.baselineId,
    baselinePath: request.baselinePath,
    scannerVersion: BOUNDARY_SCANNER_VERSION,
    readFiles: coverage.readFiles,
    readBytes: coverage.readBytes,
    elapsedMs: coverage.elapsedMs,
    coverage
  };
}
function snapshotBoundary(request) {
  const normalized = normalizeRequest({ ...request, operation: "snapshot" });
  const result = scan(normalized);
  const document = snapshotDocument(normalized, result);
  writeExclusive(join2(normalized.repoRoot, normalized.baselinePath), `${JSON.stringify(document, null, 2)}
`);
  return {
    ...summaryFromCoverage("snapshot", normalized, result.coverage),
    ok: result.coverage.complete,
    fingerprint: document.fingerprint
  };
}
function readBaseline(normalized) {
  const path = join2(normalized.repoRoot, normalized.baselinePath);
  const meta = lstatIfPresent(path);
  if (!meta)
    fail2("baselinePath does not exist");
  if (meta.isSymbolicLink() || !meta.isFile())
    fail2("baselinePath must be a regular file");
  if (meta.size > MAX_BOUNDARY_BASELINE_BYTES)
    fail2(`boundary baseline exceeds ${MAX_BOUNDARY_BASELINE_BYTES} bytes`);
  let baseline;
  try {
    baseline = JSON.parse(readBoundedUtf8(path, MAX_BOUNDARY_BASELINE_BYTES, "boundary baseline"));
  } catch (error) {
    fail2(`baseline JSON is invalid: ${error.message}`);
  }
  if (baseline.schemaVersion !== BOUNDARY_SCHEMA_VERSION || baseline.scannerVersion !== BOUNDARY_SCANNER_VERSION)
    fail2("baseline schema or scanner version mismatch");
  if (baseline.baselineId !== normalized.baselineId)
    fail2("baselineId does not match baseline file");
  if (normalized.expectedBaselineFingerprint && baseline.fingerprint !== normalized.expectedBaselineFingerprint)
    fail2("baseline fingerprint does not match expectedBaselineFingerprint");
  if (baseline.repoRoot !== normalized.repoRoot || baseline.cwd !== normalized.cwd || baseline.requestFingerprint !== normalized.requestFingerprint)
    fail2("baseline identity does not match request");
  if (!baseline.coverage?.complete)
    fail2("baseline coverage is incomplete");
  const body = { ...baseline };
  delete body.fingerprint;
  if (baseline.fingerprint !== `sha256:${sha256(stableJson(body))}`)
    fail2("baseline fingerprint mismatch");
  return baseline;
}
function compareBoundary(request) {
  const normalized = normalizeRequest({ ...request, operation: "compare" });
  if (!normalized.expectedBaselineFingerprint)
    fail2("compare requires expectedBaselineFingerprint");
  const baseline = readBaseline(normalized);
  const result = scan(normalized);
  const before = new Map(baseline.entries.map((entry) => [entry.path, entry]));
  const after = new Map(result.entries.map((entry) => [entry.path, entry]));
  const created = [];
  const removed = [];
  const typeChanged = [];
  const contentChanged = [];
  for (const path of [...new Set([...before.keys(), ...after.keys()])].sort()) {
    const a = before.get(path);
    const b = after.get(path);
    if (!a)
      created.push(path);
    else if (!b)
      removed.push(path);
    else if (a.type !== b.type)
      typeChanged.push(path);
    else if (a.type === "file" && (a.size !== b.size || a.digest !== b.digest))
      contentChanged.push(path);
    else if (a.type === "symlink" && a.target !== b.target)
      contentChanged.push(path);
  }
  const changed = [...created, ...removed, ...typeChanged, ...contentChanged];
  const allowed = changed.filter((path) => isAllowed(path, normalized.pathAllowlist));
  const outOfScope = changed.filter((path) => !isAllowed(path, normalized.pathAllowlist));
  const generatedRootChanges = [];
  for (const beforeRoot of baseline.generatedRoots) {
    const afterRoot = result.generatedRoots.find((root) => root.path === beforeRoot.path);
    if (!afterRoot)
      continue;
    if (beforeRoot.exists && !afterRoot.exists || beforeRoot.exists && afterRoot.exists && !sameIdentity(beforeRoot.identity, afterRoot.identity))
      generatedRootChanges.push(beforeRoot.path);
  }
  const coverage = result.coverage;
  const ok = coverage.complete && outOfScope.length === 0 && generatedRootChanges.length === 0;
  return {
    ...summaryFromCoverage("compare", normalized, coverage),
    ok,
    fingerprint: baseline.fingerprint,
    changes: {
      created,
      removed,
      typeChanged,
      contentChanged,
      allowed,
      outOfScope,
      generatedRootChanges
    }
  };
}
function parseBoundaryRequest(path) {
  let value;
  try {
    value = JSON.parse(readBoundedUtf8(path, MAX_BOUNDARY_REQUEST_BYTES, "boundary request"));
  } catch (error) {
    fail2(`boundary request is invalid: ${error.message}`);
  }
  if (!value || typeof value !== "object")
    fail2("boundary request must be a JSON object");
  return value;
}
function boundaryCli(operation, requestPath) {
  const request = parseBoundaryRequest(requestPath);
  return operation === "snapshot" ? snapshotBoundary(request) : compareBoundary(request);
}

// src/reader.ts
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname as dirname2, join as join3 } from "node:path";
function findRunsRoot(startDir = process.cwd()) {
  let dir = startDir;
  for (;; ) {
    const candidate = join3(dir, ".claude", "workflow-runs");
    if (existsSync(candidate))
      return candidate;
    const parent = dirname2(dir);
    if (parent === dir)
      return null;
    dir = parent;
  }
}
function loadState(runDir) {
  const raw = readFileSync(join3(runDir, "state.json"), "utf8");
  try {
    return JSON.parse(raw);
  } catch (e) {
    throw new Error(`state.json 解析失败: ${e.message}`);
  }
}
function loadOutputs(runDir) {
  const out = new Map;
  const dir = join3(runDir, "outputs");
  if (!existsSync(dir))
    return out;
  for (const f of readdirSync(dir)) {
    if (!f.endsWith(".txt"))
      continue;
    const label = f.slice(0, -".txt".length);
    out.set(label, readFileSync(join3(dir, f), "utf8"));
  }
  return out;
}
function replacePlaceholders(value, outputs) {
  if (typeof value === "string") {
    const m = /^\$\{([^}]+)\}$/.exec(value);
    if (m && outputs.has(m[1]))
      return outputs.get(m[1]);
    return value;
  }
  if (Array.isArray(value))
    return value.map((v) => replacePlaceholders(v, outputs));
  if (value && typeof value === "object") {
    const obj = {};
    for (const [k, v] of Object.entries(value)) {
      obj[k] = replacePlaceholders(v, outputs);
    }
    return obj;
  }
  return value;
}
function loadJournal(runDir) {
  const path = join3(runDir, "journal.jsonl");
  if (!existsSync(path))
    return [];
  const results = [];
  for (const line of readFileSync(path, "utf8").split(`
`)) {
    const t = line.trim();
    if (!t)
      continue;
    try {
      const entry = JSON.parse(t);
      const r = entry.result;
      if (!r)
        continue;
      const kind = r.kind ?? "ok";
      results.push({
        seq: entry.seq ?? results.length + 1,
        kind,
        output: r.output,
        tokens: r.tokenCount ?? r.usage?.outputTokens,
        tools: r.toolCount,
        durationMs: r.durationMs,
        phase: r.phase,
        reason: r.reason,
        detail: r.detail
      });
    } catch {}
  }
  results.sort((a, b) => a.seq - b.seq);
  return results;
}
function fmtDuration(start, end) {
  if (!start || !end)
    return "-";
  const ms = Date.parse(end) - Date.parse(start);
  if (Number.isNaN(ms))
    return "-";
  if (ms < 1000)
    return `${ms}ms`;
  if (ms < 60000)
    return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60000);
  const s = Math.round(ms % 60000 / 1000);
  return `${m}m${String(s).padStart(2, "0")}s`;
}
function fmtNum(n) {
  return n === undefined ? "-" : n.toLocaleString();
}
function fmtVal(v) {
  if (v === undefined || v === null)
    return "-";
  if (typeof v === "string")
    return v;
  return JSON.stringify(v);
}
function agentSummary(a) {
  const out = fmtVal(a.output);
  const first = out.split(`
`).find((l) => l.trim().length > 0) ?? "";
  return first.slice(0, 80) || out.slice(0, 80);
}
function renderReturnValue(rv, outputs) {
  const replaced = replacePlaceholders(rv, outputs);
  if (replaced === undefined || replaced === null) {
    console.log("  (无 return value)");
  } else if (typeof replaced === "string") {
    console.log(replaced);
  } else {
    for (const [k, v] of Object.entries(replaced)) {
      console.log(`### ${k}
`);
      if (typeof v === "string") {
        console.log(v.length > 0 ? v : "  (空)");
      } else {
        console.log(JSON.stringify(v, null, 2));
      }
      console.log();
    }
  }
  return replaced;
}
function resolveRunDir(runId) {
  const root = findRunsRoot();
  if (!root) {
    throw new Error("未找到 .claude/workflow-runs 目录（当前目录及其父目录均无）。请在仓库内运行。");
  }
  if (runId.includes("..") || runId.includes("/") || runId.includes("\\")) {
    throw new Error(`非法 runId（含路径字符）: ${runId}`);
  }
  const runDir = join3(root, runId);
  if (!existsSync(join3(runDir, "state.json"))) {
    throw new Error(`未找到运行 ${runId}：${runDir}（可用 peri-workflow list 查看已有 run）`);
  }
  return runDir;
}
function reportRun(runId, short, json) {
  let runDir;
  try {
    runDir = resolveRunDir(runId);
  } catch (e) {
    console.error(e.message);
    process.exit(1);
  }
  let state;
  try {
    state = loadState(runDir);
  } catch (e) {
    console.error(`读取运行 ${runId} 失败: ${e.message}`);
    process.exit(1);
  }
  const outputs = loadOutputs(runDir);
  const agents = loadJournal(runDir);
  if (json) {
    const result = {
      run_id: state.run_id,
      workflow_name: state.workflow_name,
      status: state.status,
      error: state.error ?? null,
      started_at: state.started_at ?? null,
      finished_at: state.finished_at ?? null,
      duration: fmtDuration(state.started_at, state.finished_at),
      return_value: replacePlaceholders(state.return_value, outputs),
      outputs: Object.fromEntries(outputs),
      agents,
      run_dir: runDir
    };
    console.log(JSON.stringify(result, null, 2));
    return;
  }
  console.log(`# Workflow Run ${state.run_id} — ${state.workflow_name}`);
  console.log(`status: ${state.status}${state.error ? ` | error: ${state.error}` : ""}`);
  console.log(`duration: ${fmtDuration(state.started_at, state.finished_at)}`);
  console.log(`run 目录: .claude/workflow-runs/${state.run_id}/
`);
  if (state.error) {
    console.log(`## Error

${state.error}
`);
  }
  console.log(`## Return value
`);
  if (state.return_value !== undefined && state.return_value !== null) {
    renderReturnValue(state.return_value, outputs);
  } else {
    console.log("  (无 return value)");
  }
  if (agents.length > 0) {
    console.log(`## Agents (${agents.length})
`);
    console.log("| # | phase | status | tokens | tools | 耗时 | 摘要 |");
    console.log("|---|-------|--------|-------:|------:|-----:|-------|");
    for (const a of agents) {
      const phase = a.phase ?? "-";
      const status = a.kind === "ok" ? "ok" : a.kind === "dead" ? `dead${a.reason ? ` (${a.reason})` : ""}` : "skipped";
      const dur = a.durationMs === undefined ? "-" : `${(a.durationMs / 1000).toFixed(1)}s`;
      console.log(`| ${a.seq} | ${phase} | ${status} | ${fmtNum(a.tokens)} | ${fmtNum(a.tools)} | ${dur} | ${agentSummary(a).replace(/\|/g, "\\|")} |`);
    }
    if (!short) {
      console.log();
      for (const a of agents) {
        console.log(`--- Agent ${a.seq}${a.phase ? ` (${a.phase})` : ""} [${a.kind}] ---`);
        if (a.kind === "dead") {
          console.log(`reason: ${a.reason ?? "-"}${a.detail ? `
detail: ${a.detail}` : ""}`);
        } else if (a.kind === "skipped") {
          console.log("(skipped)");
        } else {
          console.log(fmtVal(a.output));
        }
        console.log();
      }
    }
  } else {
    console.log("(journal 为空——无 agent 调用或运行过早失败)");
  }
}
function listRuns(json) {
  const root = findRunsRoot();
  if (!root) {
    console.error("未找到 .claude/workflow-runs 目录");
    process.exit(1);
  }
  const runs = [];
  for (const d of readdirSync(root)) {
    const statePath = join3(root, d, "state.json");
    if (!existsSync(statePath))
      continue;
    try {
      const st = loadState(join3(root, d));
      runs.push({
        ...st,
        duration: fmtDuration(st.started_at, st.finished_at),
        dir: d
      });
    } catch {}
  }
  runs.sort((a, b) => (a.finished_at ?? "").localeCompare(b.finished_at ?? ""));
  if (json) {
    console.log(JSON.stringify(runs, null, 2));
    return;
  }
  console.log(`# Workflow runs (${runs.length})
`);
  console.log("| run_id | workflow | status | 时长 | finished_at |");
  console.log("|--------|----------|--------|------|-------------|");
  for (const r of runs) {
    console.log(`| ${r.run_id} | ${r.workflow_name} | ${r.status} | ${r.duration} | ${r.finished_at ?? "-"} |`);
  }
  console.log(`
读取单个 run：peri-workflow read <run_id>`);
}

// node_modules/@claude-code-best/workflow-engine/dist/constants.js
var WORKFLOW_DIR_NAME = ".claude/workflows";
var WORKFLOW_SCRIPT_EXTENSIONS = [".ts", ".js", ".mjs"];
var DEFAULT_MAX_CONCURRENCY = 3;
var MAX_CONCURRENCY_CAP = 16;
var MAX_TOTAL_AGENTS = 1000;
var MAX_ITEMS_PER_CALL = 4096;

// node_modules/@claude-code-best/workflow-engine/dist/ports.js
var HOST_HANDLE = Symbol("workflow.hostHandle");
function createHostHandle(bundle) {
  return { [HOST_HANDLE]: bundle };
}

// node_modules/@claude-code-best/workflow-engine/dist/agentAdapter.js
class AdapterNotFoundError extends Error {
  constructor(message) {
    super(message);
    this.name = "AdapterNotFoundError";
  }
}

class AgentAdapterRegistry {
  adapters = new Map;
  rules = [];
  defaultId = null;
  register(adapter) {
    this.adapters.set(adapter.id, adapter);
    return this;
  }
  default(adapterId) {
    this.defaultId = adapterId;
    return this;
  }
  route(rule) {
    this.rules.push(rule);
    return this;
  }
  has(id) {
    return this.adapters.has(id);
  }
  get(id) {
    return this.adapters.get(id);
  }
  resolve(params) {
    for (const rule of this.rules) {
      if (matchRule(rule, params)) {
        const hit = this.adapters.get(rule.adapter);
        if (hit)
          return hit;
      }
    }
    if (this.defaultId) {
      const fallback = this.adapters.get(this.defaultId);
      if (fallback)
        return fallback;
    }
    throw new AdapterNotFoundError(`No adapter matched (rules=${this.rules.length}, default=${this.defaultId ?? "none"})`);
  }
  async initializeAll() {
    for (const a of this.adapters.values()) {
      await a.initialize?.();
    }
  }
  async disposeAll() {
    for (const a of this.adapters.values()) {
      await a.dispose?.();
    }
  }
}
function matchRule(rule, params) {
  if (rule.kind === "agentType")
    return params.agentType === rule.agentType;
  if (rule.kind === "model") {
    return typeof params.model === "string" && params.model.startsWith(rule.pattern);
  }
  return rule.match(params);
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/concurrency.js
class Semaphore {
  available;
  waiters = [];
  constructor(permits) {
    this.available = Math.max(1, Math.floor(permits));
  }
  async acquire(signal) {
    if (signal?.aborted) {
      throw new Error("Semaphore.acquire aborted (signal already aborted)");
    }
    if (this.available > 0) {
      this.available -= 1;
      return () => this.release();
    }
    return new Promise((resolve, reject) => {
      const onAbort = () => {
        const idx = this.waiters.indexOf(entry);
        if (idx >= 0)
          this.waiters.splice(idx, 1);
        reject(new Error("Semaphore.acquire aborted"));
      };
      const wake = () => {
        signal?.removeEventListener("abort", onAbort);
        resolve(() => this.release());
      };
      const entry = {
        wake,
        cleanup: () => signal?.removeEventListener("abort", onAbort)
      };
      signal?.addEventListener("abort", onAbort, { once: true });
      this.waiters.push(entry);
    });
  }
  release() {
    const next = this.waiters.shift();
    if (next) {
      next.wake();
    } else {
      this.available += 1;
    }
  }
}
function clampMaxConcurrency(n) {
  if (n === undefined || Number.isNaN(n))
    return DEFAULT_MAX_CONCURRENCY;
  return Math.max(1, Math.min(Math.trunc(n), MAX_CONCURRENCY_CAP));
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/script.js
class ScriptError extends Error {
  constructor(message) {
    super(message);
    this.name = "ScriptError";
  }
}
var META_RE = /export\s+const\s+meta\s*=\s*/;
function extractMeta(source) {
  const match = META_RE.exec(source);
  if (!match)
    return { meta: null, body: source };
  let i = match.index + match[0].length;
  while (i < source.length && /\s/.test(source[i]))
    i++;
  if (source[i] !== "{") {
    throw new ScriptError("meta must be an object literal `{ ... }`");
  }
  let depth = 0;
  const start = i;
  let inStr = null;
  for (;i < source.length; i++) {
    const ch = source[i];
    if (inStr) {
      if (ch === "\\") {
        i++;
        continue;
      }
      if (ch === inStr)
        inStr = null;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") {
      inStr = ch;
      continue;
    }
    if (ch === "{")
      depth++;
    else if (ch === "}") {
      depth--;
      if (depth === 0) {
        i++;
        break;
      }
    }
  }
  if (depth !== 0)
    throw new ScriptError("meta literal braces are not closed");
  const literal = source.slice(start, i);
  let metaObj;
  try {
    metaObj = new Function(`return (${literal})`)();
  } catch (e) {
    throw new ScriptError(`meta must be a plain literal (no variable/function calls/interpolation): ${e.message}`);
  }
  const meta = validateMeta(metaObj);
  const body = source.slice(0, match.index) + source.slice(i).replace(/^[ \t]*;[ \t]*\n/, `
`);
  return { meta, body };
}
function validateMeta(v) {
  if (typeof v !== "object" || v === null || Array.isArray(v)) {
    throw new ScriptError("meta must be an object");
  }
  const o = v;
  if (typeof o.name !== "string" || typeof o.description !== "string") {
    throw new ScriptError("meta must include string name and description");
  }
  return o;
}

class NonDeterministicError extends Error {
  constructor(fn) {
    super(`${fn} is not available in workflow scripts (would break resume determinism). Pass timestamps/random seeds via args.`);
    this.name = "NonDeterministicError";
  }
}
function sandboxDate() {
  const fn = function(...args) {
    if (args.length === 0)
      throw new NonDeterministicError("Date.now()/new Date()");
    return new Date(...args);
  };
  fn.now = () => {
    throw new NonDeterministicError("Date.now()");
  };
  fn.parse = Date.parse;
  fn.UTC = Date.UTC;
  return fn;
}
function sandboxMath() {
  return new Proxy(Math, {
    get(target, prop, receiver) {
      if (prop === "random") {
        return () => {
          throw new NonDeterministicError("Math.random()");
        };
      }
      return Reflect.get(target, prop, receiver);
    }
  });
}
var AsyncFunction = Object.getPrototypeOf(async function() {}).constructor;
function assertScriptBody(body) {
  if (/^\s*import\b/m.test(body)) {
    throw new ScriptError("workflow scripts are the body of new AsyncFunction (not ESM modules); import is not supported. " + "agent / parallel / pipeline / phase / log / workflow / args / budget are injected as parameters — use them directly.");
  }
  if (/\bimport\s*\(/m.test(body)) {
    throw new ScriptError("dynamic import(...) is forbidden in workflow scripts: it bypasses the Date/Math sandbox and breaks resume determinism. " + "The sandbox does not guarantee security (same trust level as the LLM), but explicit escapes are prohibited. Inject external dependencies via args.");
  }
  if (/^\s*export\b/m.test(body)) {
    throw new ScriptError("workflow scripts allow only one export const meta = {...} (already extracted by the engine). " + "Remove other export / export default statements; use top-level return for the result.");
  }
}
function parseScript(source) {
  const { meta, body } = extractMeta(source);
  assertScriptBody(body);
  let fn;
  try {
    fn = new AsyncFunction("agent", "parallel", "pipeline", "phase", "log", "workflow", "args", "budget", "Date", "Math", body);
  } catch (e) {
    throw new ScriptError(`Script syntax error: ${e.message}`);
  }
  const sandboxedDate = sandboxDate();
  const sandboxedMath = sandboxMath();
  return {
    meta,
    async execute(hooks, args, budget) {
      return fn(hooks.agent, hooks.parallel, hooks.pipeline, hooks.phase, hooks.log, hooks.workflow, args, budget, sandboxedDate, sandboxedMath);
    }
  };
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/journal.js
import { createHash as createHash3 } from "node:crypto";
function canonicalParams(params) {
  const { label: _label, phase: _phase, ...rest } = params;
  const keys = Object.keys(rest).sort();
  const sorted = {};
  for (const k of keys)
    sorted[k] = rest[k];
  return JSON.stringify(sorted);
}
function agentCallKey(prompt, params) {
  return createHash3("sha256").update(prompt + `
` + canonicalParams(params)).digest("hex");
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/budget.js
class BudgetExhaustedError extends Error {
  constructor() {
    super("workflow token budget exhausted (budget.total reached the cap)");
    this.name = "BudgetExhaustedError";
  }
}

class Budget {
  total;
  spentTokens = 0;
  constructor(total) {
    this.total = total;
  }
  spent() {
    return this.spentTokens;
  }
  remaining() {
    return this.total == null ? Infinity : Math.max(0, this.total - this.spentTokens);
  }
  addOutputTokens(n) {
    if (n > 0)
      this.spentTokens += n;
  }
  assertCanSpend() {
    if (this.total != null && this.spentTokens >= this.total) {
      throw new BudgetExhaustedError;
    }
  }
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/namedWorkflows.js
import { readFile, readdir } from "node:fs/promises";
import { parse, resolve as resolve2 } from "node:path";

// node_modules/@claude-code-best/workflow-engine/dist/engine/paths.js
import { resolve, sep as sep3 } from "node:path";
function containsPath(base, target) {
  const resolvedBase = resolve(base);
  const resolvedTarget = resolve(resolvedBase, target);
  if (resolvedTarget === resolvedBase)
    return true;
  return resolvedTarget.startsWith(resolvedBase + sep3);
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/namedWorkflows.js
async function resolveNamedWorkflow(workflowDir, name) {
  for (const ext of WORKFLOW_SCRIPT_EXTENSIONS) {
    const p = resolve2(workflowDir, name + ext);
    if (!containsPath(workflowDir, p))
      return null;
    try {
      return { path: p, content: await readFile(p, "utf-8") };
    } catch {}
  }
  return null;
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/errors.js
class WorkflowError extends Error {
  constructor(message) {
    super(message);
    this.name = "WorkflowError";
  }
}

class WorkflowAbortedError extends Error {
  constructor() {
    super("workflow has been aborted");
    this.name = "WorkflowAbortedError";
  }
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/context.js
function createSharedResources(budgetTotal, maxConcurrency) {
  return {
    semaphore: new Semaphore(clampMaxConcurrency(maxConcurrency)),
    budget: new Budget(budgetTotal),
    agentCountBox: { value: 0 },
    agentIdSeq: { value: 0 },
    depth: 0
  };
}
function createEngineContext(opts) {
  const resources = createSharedResources(opts.budgetTotal, opts.maxConcurrency);
  return {
    ports: opts.ports,
    host: opts.host,
    signal: opts.signal,
    runId: opts.runId,
    workflowName: opts.workflowName,
    cwd: opts.cwd,
    resources,
    journal: opts.journal ? [...opts.journal] : [],
    journalIndex: 0,
    journalInvalidated: false,
    currentPhase: null
  };
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/hooks.js
function makeHooks(ctx, runSubWorkflow) {
  const emit = (init) => {
    ctx.ports.progressEmitter.emit({
      runId: ctx.runId,
      ...init
    });
  };
  const agent = async (prompt, opts = {}) => {
    const r = ctx.resources;
    if (r.agentCountBox.value >= MAX_TOTAL_AGENTS) {
      throw new WorkflowError(`workflow exceeds total agent cap (${MAX_TOTAL_AGENTS})`);
    }
    const agentId2 = r.agentIdSeq.value++;
    const params = { prompt, ...opts };
    const key = agentCallKey(prompt, params);
    const label = opts.label;
    const phase2 = opts.phase ?? ctx.currentPhase ?? undefined;
    if (!ctx.journalInvalidated && ctx.journalIndex < ctx.journal.length) {
      const entry = ctx.journal[ctx.journalIndex];
      if (entry.key === key) {
        ctx.journalIndex++;
        emit({
          type: "agent_done",
          agentId: agentId2,
          label,
          phase: phase2,
          result: entry.result
        });
        return resultToOutput(entry.result);
      }
      ctx.journalInvalidated = true;
      ctx.journal = ctx.journal.slice(0, ctx.journalIndex);
      await ctx.ports.journalStore.truncate(ctx.runId);
    }
    let release;
    try {
      release = await ctx.resources.semaphore.acquire(ctx.signal);
    } catch {
      throw new WorkflowAbortedError;
    }
    try {
      if (ctx.signal.aborted)
        throw new WorkflowAbortedError;
      r.budget.assertCanSpend();
      const pending = ctx.ports.taskRegistrar.pendingAction(ctx.runId);
      if (pending?.kind === "skip") {
        const result2 = { kind: "skipped" };
        emit({ type: "agent_done", agentId: agentId2, label, phase: phase2, result: result2 });
        return null;
      }
      ctx.resources.agentCountBox.value++;
      emit({ type: "agent_started", agentId: agentId2, label, phase: phase2 });
      const registry = ctx.ports.agentAdapterRegistry;
      const onProgress = (update) => {
        emit({ type: "agent_progress", agentId: agentId2, label, phase: phase2, ...update });
      };
      const adapterCtx = registry ? {
        host: ctx.host,
        signal: ctx.signal,
        runId: ctx.runId,
        agentId: agentId2,
        onProgress,
        ...ctx.ports.taskRegistrar.registerAgentAbort ? {
          registerAgentAbort: (id, ac) => {
            ctx.ports.taskRegistrar.registerAgentAbort?.(ctx.runId, id, ac);
          }
        } : {},
        ...ctx.ports.taskRegistrar.unregisterAgentAbort ? {
          unregisterAgentAbort: (id) => {
            ctx.ports.taskRegistrar.unregisterAgentAbort?.(ctx.runId, id);
          }
        } : {}
      } : null;
      const adapter = registry ? registry.resolve(params) : null;
      const invokeBackend = () => adapter ? adapter.run(params, adapterCtx) : ctx.ports.agentRunner.runAgentToResult(params, ctx.host);
      let result;
      try {
        result = await invokeBackend();
        if (result.kind === "dead") {
          const detailStr = typeof result.detail === "string" ? result.detail : "";
          ctx.ports.logger.warn?.(`agent "${label ?? `#${agentId2}`}" returned dead` + (result.reason ? ` (${result.reason})` : "") + (detailStr ? `: ${detailStr.slice(0, 150)}` : "") + "; retrying once");
          result = await invokeBackend();
        }
      } catch (e) {
        if (e instanceof WorkflowAbortedError)
          throw e;
        const eMsg = e instanceof Error ? e.message : String(e);
        ctx.ports.logger.warn?.(`agent "${label ?? `#${agentId2}`}" threw (${eMsg}); retrying once`);
        try {
          result = await invokeBackend();
        } catch (e2) {
          if (e2 instanceof WorkflowAbortedError)
            throw e2;
          result = {
            kind: "dead",
            reason: "runagent-threw",
            detail: e2 instanceof Error ? e2.message : String(e2)
          };
        }
      }
      if (result.kind === "ok") {
        ctx.resources.budget.addOutputTokens(result.usage.outputTokens);
      }
      emit({ type: "agent_done", agentId: agentId2, label, phase: phase2, result });
      const entry = { key, seq: agentId2, result };
      ctx.journal.push(entry);
      ctx.journalIndex++;
      await ctx.ports.journalStore.append(ctx.runId, entry);
      return resultToOutput(result);
    } finally {
      release();
    }
  };
  const parallel = async (thunks) => {
    if (thunks.length > MAX_ITEMS_PER_CALL) {
      throw new WorkflowError(`parallel exceeds the per-call items cap (${MAX_ITEMS_PER_CALL})`);
    }
    return Promise.all(thunks.map(async (t, i) => {
      try {
        return await t();
      } catch (e) {
        ctx.ports.logger.warn?.(`parallel thunk #${i} failed: ${e.message}`);
        return null;
      }
    }));
  };
  const pipeline = async (items, ...stages) => {
    if (items.length > MAX_ITEMS_PER_CALL) {
      throw new WorkflowError(`pipeline exceeds the per-call items cap (${MAX_ITEMS_PER_CALL})`);
    }
    return Promise.all(items.map(async (item, index) => {
      try {
        let prev = item;
        for (const stage of stages) {
          prev = await stage(prev, item, index);
        }
        return prev;
      } catch (e) {
        ctx.ports.logger.warn?.(`pipeline item #${index} failed: ${e.message}`);
        return null;
      }
    }));
  };
  const phase = (title) => {
    if (ctx.currentPhase) {
      emit({ type: "phase_done", phase: ctx.currentPhase });
    }
    ctx.currentPhase = title;
    emit({ type: "phase_started", phase: title });
  };
  const log = (message) => {
    emit({ type: "log", message });
  };
  const workflow = async (nameOrRef, args) => {
    if (ctx.resources.depth >= 1) {
      throw new WorkflowError("workflow() nesting allows only one level");
    }
    const sub = typeof nameOrRef === "string" ? { name: nameOrRef } : { scriptPath: nameOrRef.scriptPath };
    return runSubWorkflow({ ...sub, args });
  };
  return { agent, parallel, pipeline, phase, log, workflow };
}
function resultToOutput(result) {
  return result.kind === "ok" ? result.output : null;
}

// node_modules/@claude-code-best/workflow-engine/dist/engine/runWorkflow.js
import { readFile as readFile2 } from "node:fs/promises";
import { join as join4 } from "node:path";
async function runWorkflow(opts) {
  const { ports } = opts;
  let parsed;
  try {
    parsed = parseScript(opts.script);
  } catch (e) {
    const error = e.message;
    ports.progressEmitter.emit({
      type: "run_done",
      runId: opts.runId,
      status: "failed",
      error
    });
    return { status: "failed", error };
  }
  const workflowName = opts.workflowName ?? parsed.meta?.name ?? "workflow";
  let journal = [];
  let journalInvalidated = false;
  if (opts.resume && !opts.scriptChanged) {
    journal = await ports.journalStore.read(opts.runId);
  } else if (opts.scriptChanged) {
    await ports.journalStore.truncate(opts.runId);
    journalInvalidated = true;
  }
  const ctx = createEngineContext({
    ports,
    host: opts.host,
    signal: opts.signal,
    runId: opts.runId,
    workflowName,
    cwd: opts.cwd,
    budgetTotal: opts.budgetTotal,
    maxConcurrency: opts.maxConcurrency,
    journal
  });
  if (journalInvalidated)
    ctx.journalInvalidated = true;
  ports.progressEmitter.emit({
    type: "run_started",
    runId: opts.runId,
    workflowName,
    meta: parsed.meta
  });
  const runSubWorkflow = async (sub) => {
    const script = await resolveSubScript(sub, opts.cwd);
    let subParsed;
    try {
      subParsed = parseScript(script);
    } catch (e) {
      throw new WorkflowError(`Sub-workflow script error: ${e.message}`);
    }
    const prevDepth = ctx.resources.depth;
    ctx.resources.depth += 1;
    try {
      const subHooks = makeHooks(ctx, runSubWorkflow);
      return await subParsed.execute(subHooks, sub.args, ctx.resources.budget);
    } finally {
      ctx.resources.depth = prevDepth;
    }
  };
  const hooks = makeHooks(ctx, runSubWorkflow);
  const emitTerminalPhaseDone = () => {
    if (!ctx.currentPhase)
      return;
    ports.progressEmitter.emit({
      type: "phase_done",
      runId: opts.runId,
      phase: ctx.currentPhase
    });
  };
  let result;
  try {
    const returnValue = await parsed.execute(hooks, opts.args, ctx.resources.budget);
    result = { status: "completed", returnValue };
  } catch (e) {
    if (e instanceof WorkflowAbortedError) {
      result = { status: "killed" };
    } else {
      result = { status: "failed", error: e.message };
    }
  }
  emitTerminalPhaseDone();
  ports.progressEmitter.emit({
    type: "run_done",
    runId: opts.runId,
    ...result
  });
  return result;
}
async function resolveSubScript(sub, cwd) {
  if (sub.script)
    return sub.script;
  if (sub.scriptPath)
    return await readFile2(sub.scriptPath, "utf-8");
  if (sub.name) {
    const found = await resolveNamedWorkflow(join4(cwd, WORKFLOW_DIR_NAME), sub.name);
    if (!found)
      throw new WorkflowError(`Sub-workflow "${sub.name}" not found`);
    return found.content;
  }
  throw new WorkflowError("workflow() requires name or scriptPath");
}

// src/validate.ts
var OLD_API_CALL = /\bworkflow\.(agent|parallel|pipeline|phase|log)\s*\(/g;
var HAS_RETURN = /\breturn\b/;
function validateScript(source) {
  const errors = [];
  const warnings = [];
  let meta = null;
  let body = source;
  try {
    const extracted = extractMeta(source);
    meta = extracted.meta;
    body = extracted.body;
    if (!meta) {
      errors.push({
        severity: "error",
        message: "workflow 脚本必须包含 export const meta = { name, description }（宿主依赖 meta.name 标识 workflow）。请补上 meta 声明。"
      });
    }
  } catch {}
  try {
    parseScript(source);
  } catch (e) {
    errors.push({
      severity: "error",
      message: e instanceof Error ? e.message : String(e)
    });
  }
  for (const m of body.matchAll(OLD_API_CALL)) {
    errors.push({
      severity: "error",
      message: `检测到旧式调用 workflow.${m[1]}(...)：引擎注入的是顶层自由函数，请改为直接调用 ${m[1]}(...)（无需 workflow. 前缀）。`
    });
  }
  if (!HAS_RETURN.test(body)) {
    warnings.push({
      severity: "warning",
      message: "未检测到 return 语句：脚本将返回 undefined。请在顶层用 return 返回结果（引擎只允许 export const meta，结果靠顶层 return 输出）。"
    });
  }
  return { ok: errors.length === 0, meta, errors, warnings };
}

// src/cli.ts
function cliUsage() {
  console.log(`用法（CLI 子命令）:
  peri-workflow read <runId> [--short] [--json]   # 完整报告（state + return_value + agents 全量输出）
  peri-workflow list [--json]                     # 列出所有 run（按结束时间倒序）
  peri-workflow validate <script.mjs> [--json]    # 校验 workflow 脚本语法（引擎检查 + 静态补充）
  peri-workflow boundary snapshot <request.json>  # 捕获有界 filesystem baseline
  peri-workflow boundary compare <request.json>   # 比较 baseline 与当前 filesystem
  peri-workflow adlc check-stage <request.json>   # 校验 ADLC 阶段必需产物与验收身份
  peri-workflow adlc plan <request.json>          # 生成确定性 ADLC 恢复建议
  peri-workflow --help                            # 本帮助

无参数时以 JSON-RPC 模式运行（宿主集成，见 DESIGN.md）。
read/list 从当前目录向上自动定位 .claude/workflow-runs/。`);
}
function isCliCommand(cmd) {
  return cmd === "read" || cmd === "list" || cmd === "validate" || cmd === "boundary" || cmd === "adlc" || cmd === "--help" || cmd === "-h" || cmd === "help";
}
function cliMain(args) {
  const cmd = args[0];
  if (cmd === "read") {
    const runId = args.slice(1).find((a) => !a.startsWith("--"));
    if (!runId) {
      console.error("用法：peri-workflow read <runId> [--short] [--json]（--help 查看更多）");
      process.exit(1);
    }
    reportRun(runId, args.includes("--short"), args.includes("--json"));
  } else if (cmd === "list") {
    listRuns(args.includes("--json"));
  } else if (cmd === "validate") {
    validateFile(args.slice(1).find((a) => !a.startsWith("--")), args.includes("--json"));
  } else if (cmd === "boundary") {
    boundaryFile(args.slice(1));
  } else if (cmd === "adlc") {
    adlcFile(args.slice(1));
  } else {
    cliUsage();
    process.exit(0);
  }
}
function boundaryFile(args) {
  const operation = args[0];
  const requestPath = args[1];
  if (operation !== "snapshot" && operation !== "compare" || args.length !== 2) {
    console.error("用法：peri-workflow boundary snapshot|compare <request.json>");
    process.exit(1);
  }
  if (!requestPath || requestPath.startsWith("--")) {
    console.error("用法：peri-workflow boundary snapshot|compare <request.json>");
    process.exit(1);
  }
  try {
    const result = boundaryCli(operation, requestPath);
    console.log(JSON.stringify(result, null, 2));
    if (!result.ok)
      process.exit(2);
  } catch (error) {
    console.error(`boundary ${operation} failed: ${error.message}`);
    process.exit(1);
  }
}
function validateFile(file, json) {
  if (!file) {
    console.error("用法：peri-workflow validate <script.mjs> [--json]（--help 查看更多）");
    process.exit(1);
  }
  let source;
  try {
    source = readFileSync2(file, "utf8");
  } catch {
    console.error(`无法读取文件: ${file}`);
    process.exit(1);
  }
  const r = validateScript(source);
  if (json) {
    console.log(JSON.stringify({
      file,
      ok: r.ok,
      meta: r.meta,
      errors: r.errors.map((e) => e.message),
      warnings: r.warnings.map((e) => e.message)
    }, null, 2));
    if (!r.ok)
      process.exit(1);
    return;
  }
  if (r.ok && r.warnings.length === 0) {
    const name = r.meta?.name ? ` (${r.meta.name})` : "";
    console.log(`✓ ${file} 校验通过${name}`);
    return;
  }
  if (r.ok) {
    console.log(`✓ ${file} 校验通过（${r.warnings.length} 个警告）：`);
    for (const w of r.warnings)
      console.log(`  ⚠ ${w.message}`);
    return;
  }
  console.log(`✗ ${file} 校验失败（${r.errors.length} 个错误）：`);
  for (const e of r.errors)
    console.log(`  ✗ ${e.message}`);
  for (const w of r.warnings)
    console.log(`  ⚠ ${w.message}`);
  process.exit(1);
}

// src/jsonrpc.ts
import * as readline from "readline";

// src/rpc.ts
var reqId = 100;
var _msgSeq = 0;
var writeOut = (line) => process.stdout.write(line);
var pending = new Map;
function send(msg) {
  _msgSeq++;
  writeOut(JSON.stringify(msg) + `
`);
}
function waitDrain() {
  return new Promise((resolve3) => {
    if (process.stdout.writableNeedDrain) {
      process.stdout.once("drain", resolve3);
    } else {
      resolve3();
    }
  });
}
function rpcRequest(method, params) {
  const id = reqId++;
  return new Promise((resolve3, reject) => {
    pending.set(id, { resolve: resolve3, reject });
    send({ jsonrpc: "2.0", id, method, params });
  });
}
function rpcNotify(method, params) {
  send({ jsonrpc: "2.0", method, params });
}
function handleResponse(msg) {
  const entry = pending.get(msg.id);
  if (!entry)
    return;
  pending.delete(msg.id);
  if (msg.error) {
    entry.reject(msg.error);
  } else {
    entry.resolve(msg.result);
  }
}

// src/adapter.ts
var rpcAdapter = {
  id: "perihelion-rpc",
  capabilities: { structuredOutput: true, tools: true },
  async run(params, ctx) {
    try {
      return await rpcRequest("agent/run", {
        runId: ctx.runId,
        agentId: ctx.agentId,
        prompt: params.prompt,
        schema: params.schema,
        model: params.model,
        maxTokens: params.maxTokens,
        agentType: params.agentType,
        isolation: params.isolation,
        allowedTools: params.allowedTools,
        label: params.label,
        phase: params.phase
      });
    } catch (err) {
      if (typeof err === "object" && err !== null && "code" in err && err.code === -32000) {
        throw new WorkflowAbortedError;
      }
      return { kind: "dead", reason: "runagent-threw", detail: String(err) };
    }
  }
};

// src/types.ts
var WORKFLOW_PROTOCOL_VERSION = 1;
var WORKFLOW_BUILD_ID = "@peri-code/workflow@0.2.0";

// src/server.ts
var currentRunId;
var currentAbortController;
var currentCwd;
var currentBudget;
var currentResumeRunId;
var currentResumeJournal;
function parseBudgetTotal(params) {
  if (!params || typeof params !== "object" || !Object.hasOwn(params, "budgetTotal")) {
    return;
  }
  const value = params.budgetTotal;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`budgetTotal must be an integer between 1 and ${Number.MAX_SAFE_INTEGER}`);
  }
  return value;
}
function parseResumeParams(params) {
  if (!params || typeof params !== "object")
    return;
  const raw = params;
  const hasSource = Object.hasOwn(raw, "resumeFromRunId");
  const hasJournal = Object.hasOwn(raw, "resume");
  if (!hasSource && !hasJournal)
    return;
  if (!hasSource)
    return raw.resume === null || Array.isArray(raw.resume) ? undefined : "resume must be an array when provided";
  if (typeof raw.resumeFromRunId !== "string" || raw.resumeFromRunId.length === 0) {
    return "resumeFromRunId must be a non-empty string";
  }
  if (!hasJournal || !Array.isArray(raw.resume)) {
    return "resume must be an array when resumeFromRunId is present";
  }
  return;
}
function createPorts() {
  return {
    agentAdapterRegistry: new AgentAdapterRegistry().register(rpcAdapter).default("perihelion-rpc"),
    agentRunner: {
      async runAgentToResult(params, _host) {
        return { kind: "dead", reason: "unknown", detail: "agentRunner fallback — use adapterRegistry" };
      }
    },
    progressEmitter: {
      emit(event) {
        rpcNotify("progress/event", event);
      }
    },
    taskRegistrar: {
      register() {
        return { runId: currentRunId, signal: currentAbortController.signal };
      },
      complete() {},
      fail() {},
      kill() {
        currentAbortController.abort();
      },
      pendingAction() {
        return null;
      }
    },
    journalStore: {
      async read() {
        const reusable = [];
        let nextSeq = 0;
        for (const entry of [...currentResumeJournal ?? []].sort((a, b) => a.seq - b.seq)) {
          if (entry.seq !== nextSeq || entry.result.kind !== "ok")
            break;
          nextSeq += 1;
          reusable.push(entry);
        }
        return reusable.map((entry) => {
          const recovered = {
            ...entry,
            attempt: {
              runId: currentRunId,
              journalSeq: entry.seq,
              recoveredFrom: {
                runId: currentResumeRunId ?? currentRunId,
                agentId: entry.attempt?.agentId,
                journalSeq: entry.attempt?.journalSeq ?? entry.seq
              },
              consumed: true,
              disposition: "recovered"
            }
          };
          rpcNotify("journal/append", { runId: currentRunId, entry: recovered });
          return recovered;
        });
      },
      async append(runId, entry) {
        const structured = {
          ...entry,
          attempt: {
            runId,
            journalSeq: entry.seq,
            consumed: true,
            disposition: "produced"
          }
        };
        rpcNotify("journal/append", { runId, entry: structured });
      },
      async truncate(runId) {
        rpcNotify("journal/truncate", { runId });
      }
    },
    permissionGate: { isAborted: () => false },
    logger: {
      debug(msg) {
        rpcNotify("log", { level: "debug", message: msg });
      },
      event(name, meta) {
        rpcNotify("log", { level: "event", message: name, meta });
      },
      warn(msg) {
        rpcNotify("log", { level: "warn", message: msg });
      },
      error(msg) {
        rpcNotify("log", { level: "error", message: msg });
      }
    },
    hostFactory(args) {
      return {
        handle: createHostHandle(null),
        cwd: currentCwd,
        budgetTotal: currentBudget
      };
    }
  };
}
async function handleRequest(msg) {
  const { id, method, params } = msg;
  switch (method) {
    case "workflow/start": {
      const p = params;
      let budgetTotal;
      try {
        budgetTotal = parseBudgetTotal(params);
      } catch (error) {
        send({
          jsonrpc: "2.0",
          id,
          error: {
            code: -32602,
            message: error instanceof Error ? error.message : "invalid budgetTotal"
          }
        });
        return;
      }
      const resumeError = parseResumeParams(params);
      if (resumeError) {
        send({
          jsonrpc: "2.0",
          id,
          error: { code: -32602, message: resumeError }
        });
        return;
      }
      currentRunId = p.runId;
      currentCwd = p.cwd;
      currentBudget = budgetTotal ?? null;
      currentResumeRunId = p.resumeFromRunId;
      currentResumeJournal = p.resume;
      currentAbortController = new AbortController;
      send({
        jsonrpc: "2.0",
        id,
        result: {
          ok: true,
          protocolVersion: WORKFLOW_PROTOCOL_VERSION,
          buildId: WORKFLOW_BUILD_ID
        }
      });
      runWorkflowAsync(p).catch(async (err) => {
        await waitDrain();
        rpcNotify("workflow/done", {
          runId: p.runId,
          status: "failed",
          error: String(err)
        });
        await waitDrain();
        process.exit(1);
      });
      return;
    }
    case "workflow/kill": {
      currentAbortController?.abort();
      send({
        jsonrpc: "2.0",
        id,
        result: { ok: true }
      });
      return;
    }
    default:
      send({
        jsonrpc: "2.0",
        id,
        error: {
          code: -32601,
          message: `unknown method: ${method}`
        }
      });
  }
}
async function runWorkflowAsync({
  runId,
  script,
  args,
  maxConcurrency
}) {
  parseScript(script);
  const result = await runWorkflow({
    script,
    args,
    runId,
    ports: createPorts(),
    host: createHostHandle(null),
    signal: currentAbortController.signal,
    cwd: currentCwd,
    budgetTotal: currentBudget,
    maxConcurrency,
    resume: !!currentResumeJournal
  });
  await waitDrain();
  rpcNotify("workflow/done", {
    runId,
    status: result.status,
    returnValue: result.returnValue,
    error: result.error
  });
  await waitDrain();
  process.exit(0);
}

// src/jsonrpc.ts
function startJsonRpc() {
  const rl = readline.createInterface({ input: process.stdin });
  rl.on("line", (line) => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    if ("id" in msg && msg.id !== undefined && (("result" in msg) || ("error" in msg))) {
      handleResponse(msg);
      return;
    }
    if ("method" in msg && msg.method) {
      handleRequest(msg);
    }
  });
}

// src/index.ts
var args = process.argv.slice(2);
if (isCliCommand(args[0])) {
  cliMain(args);
} else {
  startJsonRpc();
}
