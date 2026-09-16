import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { compareReportFiles, CompareInputError } from "./compare.js";
import type { ReportDto } from "./report.js";

function report(overrides: Partial<ReportDto> = {}): ReportDto {
  const tool = {
    label: "Read", outerName: "Read", effectiveName: "Read", calls: 10, pairedResults: 10,
    pairedKnownResults: 10, errors: 2, unknownErrors: 0, calledThreads: 2, errorAffectedThreads: 1, errorRate: 0.2,
  };
  const rule = (id: string, threshold: string, definition: string, numerator: number, denominator: number, kind: "quality" | "observation" = "quality") => ({
    id, kind, definition, threshold, numerator, denominator, rate: denominator === 0 ? null : numerator / denominator,
    evidence_ids: [], next_verification: "inspect",
  });
  const base: ReportDto = {
    schema_version: "report.v1",
    source: { fingerprint: "sha256:base", normalizer: "normalizer-v1", metrics: "agent-metrics-v1" },
    filters: { scope: "roots", includeHidden: false, since: "2026-01-01T00:00:00Z", until: "2026-01-02T00:00:00Z" },
    totals: { threads: 2, messages: 20, calls: 10, pairedResults: 10, pairedKnownResults: 10, missingResults: 0, orphanResults: 0, duplicateCallIds: 0, duplicateResultIds: 0, unknownErrorResults: 0, explicitErrors: 2, parseIssues: 0 },
    pairing: { paired_results: 10, paired_known_results: 10, calls: 10, missing_results: 0, orphan_results: 0, duplicate_call_ids: 0, duplicate_result_ids: 0, pairing_rate: 1, known_result_rate: 1 },
    top_tool_errors: [tool], all_tools: [tool],
    result_bytes: { count: 10, giantCount: 0, p50: 12, p95: 30, errorBytes: 20 },
    candidates: [], rules: [
      rule("pairing-quality", "paired / calls", "paired results divided by calls", 10, 10),
      rule("known-result-state", "known / paired", "known results divided by paired results", 10, 10),
      rule("repeated-call", "at least 3", "repeat count", 0, 10, "observation"),
      rule("explicit-failure", "at least 2", "failure count", 1, 10, "observation"),
      rule("large-tool-output", "at least 100000", "large result count", 0, 10, "observation"),
    ],
    capabilities: {},
  };
  return { ...base, ...overrides };
}

test("compare preserves denominators, uses percentage points, and marks window changes descriptive", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-compare-"));
  const baseline = join(root, "baseline.json"); const candidate = join(root, "candidate.json");
  const nextTool = { ...report().all_tools[0]!, errors: 4, errorRate: 0.4 };
  writeFileSync(baseline, JSON.stringify(report()));
  writeFileSync(candidate, JSON.stringify(report({
    source: { fingerprint: "sha256:candidate", normalizer: "normalizer-v1", metrics: "agent-metrics-v1" },
    filters: { scope: "roots", includeHidden: false, since: "2026-02-01T00:00:00Z", until: "2026-02-02T00:00:00Z" },
    totals: { ...report().totals, explicitErrors: 4 },
    all_tools: [nextTool], top_tool_errors: [nextTool],
    rules: report().rules.map((rule) => rule.id === "explicit-failure" ? { ...rule, numerator: 2, rate: 0.2 } : rule),
  })));
  const result = compareReportFiles(baseline, candidate);
  expect(result.filters.window_comparison).toBe("descriptive-different-creation-window");
  expect(result.rules.find((rule) => rule.id === "explicit-failure")?.rate_delta_pp).toBe(10);
  expect(result.rules.find((rule) => rule.id === "explicit-failure")?.baseline.denominator).toBe(10);
  expect(result.tools[0]?.error_rate_delta_pp).toBe(20);
  expect(result.limitations.some((item) => item.includes("causal"))).toBe(true);
  expect(readFileSync(baseline, "utf8")).not.toContain("path");
});

test("compare rejects incompatible scope, malformed rates, and duplicate tools", () => {
  const root = mkdtempSync(join(tmpdir(), "peri-compare-invalid-"));
  const base = report();
  const write = (name: string, value: unknown) => { const path = join(root, name); writeFileSync(path, JSON.stringify(value)); return path; };
  expect(() => compareReportFiles(write("base.json", base), write("scope.json", { ...base, filters: { ...base.filters, scope: "all" } }))).toThrow(CompareInputError);
  expect(() => compareReportFiles(write("base2.json", base), write("negative.json", { ...base, totals: { ...base.totals, calls: -1 } }))).toThrow("non-negative");
  expect(() => compareReportFiles(write("base3.json", base), write("duplicate.json", { ...base, all_tools: [...base.all_tools, base.all_tools[0]] }))).toThrow("duplicate");
  expect(() => compareReportFiles(write("base4.json", base), write("bad-rate.json", { ...base, pairing: { ...base.pairing, pairing_rate: 0.5 } }))).toThrow("rates");
  expect(() => compareReportFiles(write("base5.json", base), write("inconsistent-pairing.json", { ...base, totals: { ...base.totals, calls: 11 } }))).toThrow("totals and pairing");
  expect(() => compareReportFiles(write("base6.json", base), write("reverse-window.json", { ...base, filters: { ...base.filters, since: "2026-02-02T00:00:00Z", until: "2026-02-01T00:00:00Z" } }))).toThrow("since must be earlier than until");
});
