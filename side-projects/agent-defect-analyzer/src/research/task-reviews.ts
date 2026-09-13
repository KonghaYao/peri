import { readFileSync, mkdirSync, writeFileSync } from "fs";
import { join } from "path";
import { computeTaskPacketHash } from "./task-packets.js";
import type { TaskPacket as SourceTaskPacket, TaskPacketBundle as SourceTaskPacketBundle } from "./task-packets.js";

export const TASK_REVIEW_SCHEMA_VERSION = 2;
export const TASK_REVIEW_RUBRIC_VERSION = "task-effectiveness-v1";

export type Dimension = "outcome" | "verification" | "constraints" | "feedback" | "strategy";
export type TaskType = "coding" | "research" | "review" | "planning" | "operation" | "conversation" | "other" | "unclear";
export type DimensionValue = string;
type DerivedGroup = "strong" | "delivered_with_gaps" | "partial" | "not_delivered" | "blocked" | "unknown";
type CaseComparison = { unit: "reviewer-pairs" } & Record<Dimension, { agreements: number; comparable: number }>;
type CaseStatus = "missing" | "single-reviewed" | "boundary-disputed" | "dimension-disputed" | "agreements";

export type TaskPacket = SourceTaskPacket;
export type TaskPacketBundle = SourceTaskPacketBundle;
export interface DimensionReview { value: DimensionValue; evidenceMessageIds: string[]; reason: string; }
export interface PromptSignal { theme: string; direction: "reinforce" | "change" | "investigate"; target: "prompt" | "runtime" | "tooling" | "unknown"; evidenceMessageIds: string[]; counterEvidenceMessageIds: string[]; rationale: string; alternativeExplanation: string; nextCheck: string; }
export interface TaskReview {
  reviewerId: string; model: string; caseId: string; packetHash: string; requestMessageIds: string[]; endMessageId: string | null;
  taskSummary: string; acceptanceCriteria: string[]; taskType: TaskType; outcome: DimensionReview; verification: DimensionReview;
  constraints: DimensionReview; feedback: DimensionReview; strategy: DimensionReview; limitations: string[]; promptSignals: PromptSignal[];
  [key: string]: unknown;
}
export interface TaskReviewInput { schemaVersion: number; rubricVersion: string; reviews: TaskReview[]; [key: string]: unknown; }

export interface TaskReviewReport {
  schemaVersion: number; rubricVersion: string; generatedAt: string;
  reviews: Array<TaskReview & { derivedGroup: DerivedGroup }>;
  reviewDimensions: Record<Dimension, Record<string, number>>;
  reviewLabelsByCase: Record<string, Record<Dimension, Record<string, number>>>;
  caseDimensionDistribution: Record<Dimension, Record<string, number>>;
  reviewGroupDistribution: Record<string, number>;
  caseGroupDistribution: Record<string, number>;
  cases: Array<{ caseId: string; reviewCount: number; status: CaseStatus; reviewers: string[]; comparison?: CaseComparison }>;
  strata: { candidates: Record<string, number>; selected: Record<string, number> };
  overall: { caseCount: number; reviewCount: number; candidateCount: number; selectedCount: number; unknownDenominators: Record<string, number> };
  promptSignals: Array<PromptSignal & { caseId: string; reviewerId: string }>;
  validation: { rejectedReviews: number; warnings: string[] };
}

export class TaskReviewInputError extends Error {}

const EXECUTION_STATUSES = new Set(["unknown", "completed", "failed", "cancelled", "timed_out", "running", "running_after_timeout"]);

const ENUMS: Record<Dimension, readonly string[]> = {
  outcome: ["delivered", "partial", "not_delivered", "blocked", "unknown"],
  verification: ["direct", "reported_only", "none", "not_applicable", "unknown"],
  constraints: ["supported", "violated", "unknown"],
  feedback: ["accepted", "corrected", "rejected", "mixed", "none_observed", "unknown"],
  strategy: ["effective", "avoidable_friction", "unknown"],
};

function readJson(path: string): unknown { try { return JSON.parse(readFileSync(path, "utf8")); } catch (error) { throw new TaskReviewInputError(`invalid JSON ${path}: ${String(error)}`); } }
function object(value: unknown, name: string): Record<string, unknown> { if (!value || typeof value !== "object" || Array.isArray(value)) throw new TaskReviewInputError(`${name} must be an object`); return value as Record<string, unknown>; }
function array(value: unknown, name: string): unknown[] { if (!Array.isArray(value)) throw new TaskReviewInputError(`${name} must be an array`); return value; }
function string(value: unknown, name: string, allowEmpty = false): string { if (typeof value !== "string" || (!allowEmpty && value.length === 0)) throw new TaskReviewInputError(`${name} must be a non-empty string`); return value; }
function strings(value: unknown, name: string, allowEmpty = false): string[] { const values = array(value, name).map((item, index) => string(item, `${name}[${index}]`, allowEmpty)); if (new Set(values).size !== values.length) throw new TaskReviewInputError(`${name} contains duplicates`); return values; }

function deriveGroup(review: TaskReview): DerivedGroup {
  if (review.outcome.value === "delivered") return review.verification.value === "direct" || review.verification.value === "not_applicable" ? review.constraints.value === "supported" && review.strategy.value === "effective" ? "strong" : "delivered_with_gaps" : "delivered_with_gaps";
  if (["partial", "not_delivered", "blocked", "unknown"].includes(review.outcome.value)) return review.outcome.value as "partial" | "not_delivered" | "blocked" | "unknown";
  return "unknown";
}
function validateDimension(value: unknown, dimension: Dimension, ids: Set<string>, sequence: Map<string, number>, endSequence: number | null, truncated: boolean): DimensionReview {
  const item = object(value, dimension); const label = item.value;
  if (typeof label !== "string" || !ENUMS[dimension].includes(label)) throw new TaskReviewInputError(`invalid ${dimension} value`);
  const evidence = strings(item.evidenceMessageIds, `${dimension}.evidenceMessageIds`);
  if (evidence.some((id) => !ids.has(id))) throw new TaskReviewInputError(`${dimension} cites a message outside the packet`);
  if (endSequence !== null && evidence.some((id) => (sequence.get(id) ?? Number.MAX_SAFE_INTEGER) > endSequence)) throw new TaskReviewInputError(`${dimension} cites evidence after endMessageId`);
  if (typeof item.reason !== "string") throw new TaskReviewInputError(`${dimension}.reason must be a string`);
  if (!["unknown", "none_observed"].includes(label) && evidence.length === 0) throw new TaskReviewInputError(`${dimension} deterministic conclusion requires evidence`);
  if (truncated && ["none", "none_observed"].includes(label)) throw new TaskReviewInputError(`${dimension} cannot use ${label} on a truncated packet`);
  return { value: label, evidenceMessageIds: evidence, reason: item.reason };
}
function validateReview(raw: unknown, packet: TaskPacket): TaskReview {
  const item = object(raw, "review"); const messages = packet.messages;
  const ids = new Set<string>();
  let previousSequence = -1;
  for (const [index, message] of messages.entries()) {
    const id = string(message.messageId, `messages[${index}].messageId`);
    if (ids.has(id)) throw new TaskReviewInputError("packet contains duplicate message IDs");
    ids.add(id);
    if (message.threadId !== packet.threadId) throw new TaskReviewInputError("packet message threadId mismatch");
    if (message.origin !== "own" && message.origin !== "inherited") throw new TaskReviewInputError("packet message origin is invalid");
    if (!Number.isInteger(message.sequence) || message.sequence <= previousSequence) throw new TaskReviewInputError("packet message sequence is not strictly increasing");
    previousSequence = message.sequence;
    if (!Array.isArray(message.results)) throw new TaskReviewInputError(`messages[${index}].results must be an array`);
    for (const [resultIndex, result] of message.results.entries()) {
      if (typeof result.isError !== "boolean" && result.isError !== null) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid isError`);
      const execution = result.execution as unknown as Record<string, unknown> | undefined;
      if (!execution || typeof execution !== "object" || Array.isArray(execution)) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] is missing execution facts`);
      if (typeof execution.status !== "string" || !EXECUTION_STATUSES.has(execution.status)) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid execution status`);
      if (typeof execution.exitCode !== "number" && execution.exitCode !== null || (typeof execution.exitCode === "number" && (!Number.isInteger(execution.exitCode) || execution.exitCode < -2147483648 || execution.exitCode > 2147483647))) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid execution exitCode`);
      if (typeof execution.hasOutputRef !== "boolean" || typeof execution.outputTruncated !== "boolean") throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid execution flags`);
      if (typeof execution.hasTaskId !== "boolean") throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid execution taskId flag`);
      if (execution.source !== "typed" && execution.source !== "legacy") throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid execution source`);
      if (execution.source === "legacy" && execution.status !== "unknown") throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] legacy execution cannot claim a known status`);
      if (execution.source === "legacy" && (execution.exitCode !== null || execution.outputTruncated || execution.hasOutputRef || execution.hasTaskId || "outputRef" in execution || "taskId" in execution)) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] legacy execution carries fabricated facts`);
      const errorStatus = ["failed", "cancelled", "timed_out", "running_after_timeout"].includes(execution.status);
      if (result.isError !== null && execution.status !== "unknown" && result.isError !== errorStatus) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] execution status conflicts with isError`);
      if ((execution.status === "completed" && execution.exitCode !== null && execution.exitCode !== 0) || (execution.status === "failed" && execution.exitCode === 0) || (["cancelled", "timed_out", "running", "running_after_timeout"].includes(execution.status) && execution.exitCode !== null)) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] execution status conflicts with exitCode`);
      if (packet.coverage.includeContent) {
        if ("taskId" in execution && typeof execution.taskId !== "string") throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid execution taskId`);
        if (execution.hasTaskId !== ("taskId" in execution && typeof execution.taskId === "string")) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] taskId flag is inconsistent`);
        if ("outputRef" in execution && typeof execution.outputRef !== "string") throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] has invalid outputRef`);
        if (execution.hasOutputRef !== ("outputRef" in execution && typeof execution.outputRef === "string")) throw new TaskReviewInputError(`messages[${index}].results[${resultIndex}] outputRef flag is inconsistent`);
      } else {
        if ("outputRef" in execution || "taskId" in execution) throw new TaskReviewInputError("metadata packet cannot carry raw execution references");
      }
    }
  }
  if (!Number.isInteger(packet.coverage.sourceMessageCount) || packet.coverage.sourceMessageCount < messages.length) throw new TaskReviewInputError("packet coverage sourceMessageCount is invalid");
  if (packet.coverage.omittedMessages !== packet.coverage.sourceMessageCount - messages.length) throw new TaskReviewInputError("packet coverage omittedMessages is inconsistent");
  if (new Set(packet.coverage.omittedFields).size !== packet.coverage.omittedFields.length || packet.coverage.omittedFields.some((field) => typeof field !== "string" || field.length === 0)) throw new TaskReviewInputError("packet coverage omittedFields is invalid");
  if (!packet.coverage.exportTruncated && (packet.coverage.omittedMessages > 0 || packet.coverage.omittedFields.length > 0)) throw new TaskReviewInputError("packet coverage omits data without exportTruncated");
  const required = ["reviewerId", "model", "caseId", "packetHash", "requestMessageIds", "endMessageId", "taskSummary", "acceptanceCriteria", "taskType", "limitations", "promptSignals"];
  for (const key of required) if (!(key in item)) throw new TaskReviewInputError(`review missing ${key}`);
  if (item.caseId !== packet.caseId || item.packetHash !== packet.packetHash) throw new TaskReviewInputError("review packet identity mismatch");
  const requests = strings(item.requestMessageIds, "requestMessageIds");
  if (requests.some((id) => !ids.has(id) || !messages.find((m) => m.messageId === id && m.role === "user" && m.origin === "own"))) throw new TaskReviewInputError("requestMessageIds must be own user messages in the packet");
  const end = item.endMessageId === null ? null : string(item.endMessageId, "endMessageId");
  if (end !== null && !ids.has(end)) throw new TaskReviewInputError("endMessageId is outside the packet");
  const sequences = new Map(messages.map((m) => [m.messageId, m.sequence]));
  if (end !== null && requests.some((id) => (sequences.get(id) ?? 0) > (sequences.get(end) ?? 0))) throw new TaskReviewInputError("endMessageId precedes request anchor");
  const truncated = messages.some((m) => m.truncated === true) || packet.coverage.exportTruncated || packet.coverage.sourceTruncatedMessages > 0 || packet.coverage.omittedMessages > 0 || packet.coverage.omittedFields.length > 0;
  const signals = array(item.promptSignals, "promptSignals").map((rawSignal) => {
    const signal = object(rawSignal, "promptSignal");
    const evidenceMessageIds = strings(signal.evidenceMessageIds, "promptSignal.evidenceMessageIds");
    const counterEvidenceMessageIds = strings(signal.counterEvidenceMessageIds, "promptSignal.counterEvidenceMessageIds");
    if (evidenceMessageIds.length === 0) throw new TaskReviewInputError("promptSignal requires evidence");
    if ([...evidenceMessageIds, ...counterEvidenceMessageIds].some((id) => !ids.has(id))) throw new TaskReviewInputError("promptSignal cites a message outside the packet");
    if (end !== null && [...evidenceMessageIds, ...counterEvidenceMessageIds].some((id) => (sequences.get(id) ?? Number.MAX_SAFE_INTEGER) > (sequences.get(end) ?? Number.MAX_SAFE_INTEGER))) throw new TaskReviewInputError("promptSignal cites evidence after endMessageId");
    if (!["reinforce", "change", "investigate"].includes(String(signal.direction)) || !["prompt", "runtime", "tooling", "unknown"].includes(String(signal.target))) throw new TaskReviewInputError("invalid promptSignal enum");
    for (const key of ["theme", "rationale", "alternativeExplanation", "nextCheck"]) if (typeof signal[key] !== "string") throw new TaskReviewInputError(`promptSignal.${key} must be a string`);
    if (!packet.coverage.includeContent) throw new TaskReviewInputError("metadata packet cannot carry prompt signals");
    return { theme: signal.theme as string, direction: signal.direction as PromptSignal["direction"], target: signal.target as PromptSignal["target"], evidenceMessageIds, counterEvidenceMessageIds, rationale: signal.rationale as string, alternativeExplanation: signal.alternativeExplanation as string, nextCheck: signal.nextCheck as string };
  });
  const reviewerId = string(item.reviewerId, "reviewerId");
  const model = string(item.model, "model");
  const taskSummary = string(item.taskSummary, "taskSummary");
  const acceptanceCriteria = strings(item.acceptanceCriteria, "acceptanceCriteria");
  const limitations = strings(item.limitations, "limitations", true);
  const taskType = string(item.taskType, "taskType") as TaskType;
  const result = { reviewerId, model, caseId: packet.caseId, packetHash: packet.packetHash, requestMessageIds: requests, endMessageId: end, taskSummary, acceptanceCriteria, taskType, limitations, promptSignals: signals } as unknown as TaskReview;
  for (const dimension of Object.keys(ENUMS) as Dimension[]) {
    const validated = validateDimension(item[dimension], dimension, ids, sequences, end === null ? null : sequences.get(end) ?? null, truncated);
    if (!packet.coverage.includeContent && validated.value !== "unknown") throw new TaskReviewInputError(`metadata packet cannot carry a determinate ${dimension} label`);
    (result as Record<string, unknown>)[dimension] = validated;
  }
  if (!["coding", "research", "review", "planning", "operation", "conversation", "other", "unclear"].includes(result.taskType)) throw new TaskReviewInputError("invalid taskType");
  if (requests.length === 0) {
    const allUnknown = (Object.keys(ENUMS) as Dimension[]).every((dimension) => (result[dimension] as DimensionReview).value === "unknown");
    if (!allUnknown || result.taskType !== "unclear" || end !== null || signals.length > 0 || !result.limitations.some((limitation) => limitation.length > 0)) throw new TaskReviewInputError("empty request anchor requires null end, no prompt signals, unclear, unknown dimensions and a limitation");
  }
  return result;
}

export function reviewTaskFiles(packetsPath: string, reviewsPath: string): TaskReviewReport {
  const bundle = object(readJson(packetsPath), "packet bundle") as unknown as TaskPacketBundle;
  const input = object(readJson(reviewsPath), "review input") as unknown as TaskReviewInput;
  if (bundle.schemaVersion !== 2 || bundle.rubricVersion !== TASK_REVIEW_RUBRIC_VERSION) throw new TaskReviewInputError("unsupported packet schema or rubric; re-export packets with schema 2");
  if (input.schemaVersion !== TASK_REVIEW_SCHEMA_VERSION || input.rubricVersion !== TASK_REVIEW_RUBRIC_VERSION) throw new TaskReviewInputError("unsupported review schema or rubric; re-run review with schema 2");
  const packets = array(bundle.packets, "packets") as TaskPacket[]; const packetByCase = new Map<string, TaskPacket>();
  for (const packet of packets) {
    const caseId = string(packet.caseId, "packet.caseId");
    if (packetByCase.has(caseId)) throw new TaskReviewInputError(`duplicate case ${caseId}`);
    if (packet.packetHash !== computeTaskPacketHash(packet)) throw new TaskReviewInputError(`packet ${caseId} hash mismatch`);
    packetByCase.set(caseId, packet);
  }
  const seen = new Set<string>(); const reviews: TaskReviewReport["reviews"] = []; const warnings: string[] = [];
  for (const raw of array(input.reviews, "reviews")) {
    const item = object(raw, "review");
    const caseId = string(item.caseId, "review.caseId");
    const packet = packetByCase.get(caseId);
    if (!packet) throw new TaskReviewInputError(`review references missing case ${caseId}`);
    const review = validateReview(raw, packet);
    const key = `${review.reviewerId}\0${review.caseId}`;
    if (seen.has(key)) throw new TaskReviewInputError(`duplicate reviewer-case ${key}`);
    seen.add(key);
    reviews.push({ ...review, derivedGroup: deriveGroup(review) });
  }
  const reviewDimensions = {} as Record<Dimension, Record<string, number>>;
  for (const dimension of Object.keys(ENUMS) as Dimension[]) {
    reviewDimensions[dimension] = {};
    for (const review of reviews) {
      const value = (review[dimension] as DimensionReview).value;
      reviewDimensions[dimension][value] = (reviewDimensions[dimension][value] ?? 0) + 1;
    }
  }
  const byCase = new Map<string, typeof reviews>(); for (const review of reviews) byCase.set(review.caseId, [...(byCase.get(review.caseId) ?? []), review]);
  const cases: TaskReviewReport["cases"] = packets.map((packet) => {
    const rs = byCase.get(packet.caseId) ?? [];
    const reviewers = rs.map((review) => review.reviewerId);

    if (rs.length === 0) {
      return { caseId: packet.caseId, reviewCount: 0, status: "missing" as const, reviewers };
    }
    if (rs.length === 1) {
      return { caseId: packet.caseId, reviewCount: 1, status: "single-reviewed" as const, reviewers };
    }

    const anchors = new Set(rs.map((review) => {
      const requestIds = [...review.requestMessageIds].sort().join("\0");
      return `${requestIds}\0${review.endMessageId ?? ""}`;
    }));
    if (anchors.size > 1) {
      return { caseId: packet.caseId, reviewCount: rs.length, status: "boundary-disputed" as const, reviewers };
    }

    const comparison = { unit: "reviewer-pairs" as const } as CaseComparison;
    for (const dimension of Object.keys(ENUMS) as Dimension[]) {
      let agreements = 0;
      let comparable = 0;
      for (let leftIndex = 0; leftIndex < rs.length; leftIndex++) {
        for (let rightIndex = leftIndex + 1; rightIndex < rs.length; rightIndex++) {
          const left = (rs[leftIndex][dimension] as DimensionReview).value;
          const right = (rs[rightIndex][dimension] as DimensionReview).value;
          if (left === "unknown" || right === "unknown") continue;
          comparable++;
          if (left === right) agreements++;
        }
      }
      comparison[dimension] = { agreements, comparable };
    }

    return { caseId: packet.caseId, reviewCount: rs.length, status: "agreements" as const, reviewers, comparison };
  });
  for (const entry of cases) {
    const rs = byCase.get(entry.caseId) ?? [];
    if (entry.status === "agreements" && (Object.keys(ENUMS) as Dimension[]).some((dimension) => new Set(rs.map((review) => (review[dimension] as DimensionReview).value)).size > 1)) entry.status = "dimension-disputed";
  }
  const reviewLabelsByCase: Record<string, Record<Dimension, Record<string, number>>> = {};
  for (const packet of packets) {
    reviewLabelsByCase[packet.caseId] = {} as Record<Dimension, Record<string, number>>;
    for (const dimension of Object.keys(ENUMS) as Dimension[]) {
      reviewLabelsByCase[packet.caseId][dimension] = {};
      for (const review of byCase.get(packet.caseId) ?? []) {
        const value = (review[dimension] as DimensionReview).value;
        reviewLabelsByCase[packet.caseId][dimension][value] = (reviewLabelsByCase[packet.caseId][dimension][value] ?? 0) + 1;
      }
    }
  }
  const reviewGroupDistribution: Record<string, number> = {};
  for (const review of reviews) reviewGroupDistribution[review.derivedGroup] = (reviewGroupDistribution[review.derivedGroup] ?? 0) + 1;
  const caseGroupDistribution: Record<string, number> = {};
  for (const entry of cases) {
    const caseReviews = byCase.get(entry.caseId) ?? [];
    let group: string;
    if (entry.status === "missing") {
      group = "unreviewed";
    } else if (entry.status === "single-reviewed") {
      group = "single_review";
    } else if (entry.status === "boundary-disputed") {
      group = "boundary_disputed";
    } else {
      const derivedGroups = new Set(caseReviews.map((review) => review.derivedGroup));
      group = derivedGroups.size === 1 ? [...derivedGroups][0] : "disputed";
    }
    caseGroupDistribution[group] = (caseGroupDistribution[group] ?? 0) + 1;
  }
  const caseDimensionDistribution = {} as Record<Dimension, Record<string, number>>;
  for (const dimension of Object.keys(ENUMS) as Dimension[]) {
    caseDimensionDistribution[dimension] = {};
    for (const entry of cases) {
      const caseReviews = byCase.get(entry.caseId) ?? [];
      let bucket = "unreviewed";
      if (caseReviews.length === 1) {
        bucket = "single_review";
      } else if (entry.status === "boundary-disputed") {
        bucket = "boundary_disputed";
      } else if (caseReviews.length > 1) {
        const labels = Object.keys(reviewLabelsByCase[entry.caseId][dimension]);
        bucket = labels.length === 1 ? labels[0] : "disputed";
      }
      caseDimensionDistribution[dimension][bucket] = (caseDimensionDistribution[dimension][bucket] ?? 0) + 1;
    }
  }
  const promptSignals = reviews.flatMap((review) => review.promptSignals.map((signal) => ({ ...signal, caseId: review.caseId, reviewerId: review.reviewerId })));
  const strata = { candidates: {}, selected: {} } as { candidates: Record<string, number>; selected: Record<string, number> }; for (const [stratum, info] of Object.entries(bundle.sampling.strata)) { strata.candidates[stratum] = info.candidateCount; strata.selected[stratum] = info.selectedCount; }
  return { schemaVersion: TASK_REVIEW_SCHEMA_VERSION, rubricVersion: TASK_REVIEW_RUBRIC_VERSION, generatedAt: new Date().toISOString(), reviews, reviewDimensions, reviewLabelsByCase, caseDimensionDistribution, reviewGroupDistribution, caseGroupDistribution, cases, strata, overall: { caseCount: packets.length, reviewCount: reviews.length, candidateCount: Object.values(strata.candidates).reduce((a, b) => a + b, 0), selectedCount: packets.length, unknownDenominators: Object.fromEntries((Object.keys(reviewDimensions) as Dimension[]).map((d) => [d, reviewDimensions[d].unknown ?? 0])) }, promptSignals, validation: { rejectedReviews: 0, warnings } };
}

export function writeTaskReviewReport(report: TaskReviewReport, outDir: string): void {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, "task-review.json"), JSON.stringify(report, null, 2) + "\n");
  const lines = ["# Task review report", "", `- Cases: ${report.overall.caseCount}`, `- Reviews: ${report.overall.reviewCount}`, "", "## Review dimension distributions", ""];
  for (const dimension of Object.keys(report.reviewDimensions) as Dimension[]) {
    const values = Object.entries(report.reviewDimensions[dimension]).map(([key, count]) => `${key}=${count}`).join(", ") || "none";
    lines.push(`- ${dimension}: ${values}`);
  }
  lines.push("", "## Case groups", "", ...Object.entries(report.caseGroupDistribution).map(([key, count]) => `- ${key}: ${count}`), "", "## Cases", "");
  for (const item of report.cases) lines.push(`- ${item.caseId}: ${item.status} (${item.reviewCount} review${item.reviewCount === 1 ? "" : "s"})`);
  writeFileSync(join(outDir, "task-review.md"), lines.join("\n") + "\n");
}
