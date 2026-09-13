import { expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { reviewTaskFiles, TaskReviewInputError } from "./task-reviews.js";
import { computeTaskPacketHash } from "./task-packets.js";
import type { TaskPacket, TaskPacketBundle } from "./task-reviews.js";

function packet(includeContent = true, caseId = "case-1"): TaskPacket {
  const threadId = `thread-${caseId}`;
  const result = { caseId, threadId, packetHash: "", ownMessageCount: 2, parentThreadId: null, childThreadIds: [], messages: [
    { messageId: "u1", threadId, sequence: 1, role: "user", origin: "own" as const, calls: [], results: [], truncated: false, isSummary: false, parseIssues: [], ...(includeContent ? { text: "do it" } : {}) },
    { messageId: "a1", threadId, sequence: 2, role: "assistant", origin: "own" as const, calls: [], results: [], truncated: false, isSummary: false, parseIssues: [], ...(includeContent ? { text: "done" } : {}) },
  ], coverage: { sourceMessageCount: 2, exportedMessageCount: 2, omittedMessages: 0, omittedFields: [], sourceTruncatedMessages: 0, exportTruncated: false, truncated: false, includeContent, byteCount: 0 } };
  result.packetHash = computeTaskPacketHash(result);
  return result;
}
function files(reviews: unknown, source = packet()): [string, string] { return filesForPackets([source], reviews); }
function filesForPackets(sources: TaskPacket[], reviews: unknown): [string, string] {
  const dir = mkdtempSync(join(tmpdir(), "task-review-"));
  const empty = { candidates: [], selected: [] };
  const ids = sources.map((source) => source.caseId);
  const strata = {
    short: { range: "1..20", candidateCount: sources.length, selectedCount: sources.length, candidateIds: ids, selectedIds: ids, ...empty, rangeHash: "x" },
    medium: { range: "21..100", candidateCount: 0, selectedCount: 0, candidateIds: [], selectedIds: [], ...empty, rangeHash: "x" },
    long: { range: ">100", candidateCount: 0, selectedCount: 0, candidateIds: [], selectedIds: [], ...empty, rangeHash: "x" },
  };
  const bundle: TaskPacketBundle = { schemaVersion: 1, rubricVersion: "task-effectiveness-v1", sampling: { seed: "x", perStratum: 1, scope: "roots", includeHidden: false, since: null, until: null, strata, noOwnMessages: 0, source: { normalizer: "x", schemaIdentity: "x", snapshot: "readonly-transaction" } }, packets: sources };
  const reviewFile = join(dir, "reviews.json");
  const packetFile = join(dir, "packets.json");
  writeFileSync(packetFile, JSON.stringify(bundle));
  writeFileSync(reviewFile, JSON.stringify({ schemaVersion: 1, rubricVersion: "task-effectiveness-v1", reviews }));
  return [packetFile, reviewFile];
}
function review(p: TaskPacket, overrides: Record<string, unknown> = {}): Record<string, unknown> { const dimension = (value: string) => ({ value, evidenceMessageIds: value === "unknown" ? [] : ["a1"], reason: "evidence" }); return { reviewerId: "r1", model: "m", caseId: p.caseId, packetHash: p.packetHash, requestMessageIds: ["u1"], endMessageId: "a1", taskSummary: "task", acceptanceCriteria: ["done"], taskType: "coding", outcome: dimension("delivered"), verification: dimension("direct"), constraints: dimension("supported"), feedback: dimension("none_observed"), strategy: dimension("effective"), limitations: [], promptSignals: [], ...overrides }; }

test("validates hash, derives strong, and reports strata", () => { const p = packet(); const [packets, reviews] = files([review(p)]); const report = reviewTaskFiles(packets, reviews); expect(report.reviews[0]?.derivedGroup).toBe("strong"); expect(report.overall.caseCount).toBe(1); expect(report.strata.candidates.short).toBe(1); });
test("aggregates reviewer groups per case and keeps pairwise comparison units explicit", () => {
  const p = packet();
  const [packets, reviews] = files([review(p), review(p, { reviewerId: "r2" })]);
  const report = reviewTaskFiles(packets, reviews);

  expect(report.reviewGroupDistribution).toEqual({ strong: 2 });
  expect(report.caseGroupDistribution).toEqual({ strong: 1 });
  expect(report.cases[0]?.status).toBe("agreements");
  expect(report.cases[0]?.comparison?.unit).toBe("reviewer-pairs");
  expect(report.cases[0]?.comparison?.outcome).toEqual({ agreements: 1, comparable: 1 });
});

test("counts each case once across unknown, single, boundary, disputed, and missing groups", () => {
  const strong = packet(true, "case-strong");
  const feedback = packet(true, "case-feedback");
  const disagreement = packet(true, "case-disagreement");
  const boundary = packet(true, "case-boundary");
  const unknown = packet(true, "case-unknown");
  const single = packet(true, "case-single");
  const missing = packet(true, "case-missing");
  const unknownDimension = { value: "unknown", evidenceMessageIds: [], reason: "not observable" };
  const [packets, reviews] = filesForPackets(
    [strong, feedback, disagreement, boundary, unknown, single, missing],
    [
      review(strong), review(strong, { reviewerId: "r2" }),
      review(feedback, { feedback: { value: "accepted", evidenceMessageIds: ["a1"], reason: "accepted" } }),
      review(feedback, { reviewerId: "r2", feedback: { value: "corrected", evidenceMessageIds: ["a1"], reason: "corrected" } }),
      review(disagreement),
      review(disagreement, { reviewerId: "r2", outcome: { value: "partial", evidenceMessageIds: ["a1"], reason: "incomplete" } }),
      review(disagreement, { reviewerId: "r3", outcome: { value: "partial", evidenceMessageIds: ["a1"], reason: "incomplete" } }),
      review(boundary), review(boundary, { reviewerId: "r2", endMessageId: null }),
      review(unknown, { outcome: unknownDimension, verification: unknownDimension, constraints: unknownDimension, feedback: unknownDimension, strategy: unknownDimension }),
      review(unknown, { reviewerId: "r2", outcome: unknownDimension, verification: unknownDimension, constraints: unknownDimension, feedback: unknownDimension, strategy: unknownDimension }),
      review(single),
    ],
  );
  const report = reviewTaskFiles(packets, reviews);

  expect(report.overall.caseCount).toBe(7);
  expect(report.overall.reviewCount).toBe(12);
  expect(report.reviewGroupDistribution).toEqual({ strong: 8, partial: 2, unknown: 2 });
  expect(report.caseGroupDistribution).toEqual({ strong: 2, disputed: 1, boundary_disputed: 1, unknown: 1, single_review: 1, unreviewed: 1 });
  expect(report.cases.find((entry) => entry.caseId === "case-feedback")?.status).toBe("dimension-disputed");
  expect(report.caseDimensionDistribution.outcome).toEqual({ delivered: 2, disputed: 1, boundary_disputed: 1, unknown: 1, single_review: 1, unreviewed: 1 });
  expect(report.caseDimensionDistribution.feedback).toEqual({ none_observed: 2, disputed: 1, boundary_disputed: 1, unknown: 1, single_review: 1, unreviewed: 1 });
  expect(report.caseDimensionDistribution.verification).toEqual({ direct: 3, boundary_disputed: 1, unknown: 1, single_review: 1, unreviewed: 1 });
  for (const distribution of Object.values(report.caseDimensionDistribution)) expect(Object.values(distribution).reduce((sum, count) => sum + count, 0)).toBe(report.overall.caseCount);
});

test("rejects wrong hash and outside evidence", () => { const p = packet(); const bad = { ...p, packetHash: "wrong" }; const [packets, reviews] = files([review(bad)]); expect(() => reviewTaskFiles(packets, reviews)).toThrow(TaskReviewInputError); const good = packet(); const [packets2, reviews2] = files([review(good, { outcome: { value: "delivered", evidenceMessageIds: ["missing"], reason: "x" } })]); expect(() => reviewTaskFiles(packets2, reviews2)).toThrow("outside the packet"); });
test("allows empty anchor when own messages do not identify a task", () => { const p = packet(); p.packetHash = computeTaskPacketHash(p); const r = review(p, { requestMessageIds: [], endMessageId: null, taskType: "unclear", limitations: ["no recognizable request"], outcome: { value: "unknown", evidenceMessageIds: [], reason: "no task anchor" }, verification: { value: "unknown", evidenceMessageIds: [], reason: "unknown" }, constraints: { value: "unknown", evidenceMessageIds: [], reason: "unknown" }, feedback: { value: "unknown", evidenceMessageIds: [], reason: "unknown" }, strategy: { value: "unknown", evidenceMessageIds: [], reason: "unknown" } }); const [packets, reviews] = files([r], p); expect(reviewTaskFiles(packets, reviews).reviews[0]?.derivedGroup).toBe("unknown"); });
test("rejects determinate, bounded, or signaled empty anchors", () => {
  const p = packet();
  const unknown = { value: "unknown", evidenceMessageIds: [], reason: "unknown" };
  const base = { requestMessageIds: [], endMessageId: null, taskType: "unclear", limitations: ["no recognizable request"], outcome: unknown, verification: unknown, constraints: unknown, feedback: unknown, strategy: unknown };
  const determinate = review(p, { ...base, outcome: { value: "delivered", evidenceMessageIds: ["a1"], reason: "found a result" } });
  const bounded = review(p, { ...base, endMessageId: "a1" });
  const signaled = review(p, { ...base, promptSignals: [{ theme: "unclear", direction: "investigate", target: "prompt", evidenceMessageIds: ["a1"], counterEvidenceMessageIds: [], rationale: "unclear", alternativeExplanation: "none", nextCheck: "inspect" }] });
  for (const candidate of [determinate, bounded, signaled]) {
    const [packets, reviews] = files([candidate], p);
    expect(() => reviewTaskFiles(packets, reviews)).toThrow("empty request anchor");
  }
});

test("rejects evidence after the observation boundary", () => { const p = packet(); const r = review(p, { outcome: { value: "delivered", evidenceMessageIds: ["a1"], reason: "after boundary" }, endMessageId: "u1" }); const [packets, reviews] = files([r], p); expect(() => reviewTaskFiles(packets, reviews)).toThrow("after endMessageId"); });
test("metadata packets cannot assert determinate labels", () => { const p = packet(false); const [packets, reviews] = files([review(p)], p); expect(() => reviewTaskFiles(packets, reviews)).toThrow("metadata packet"); });
