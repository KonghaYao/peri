import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import type { ReportDto, ReportRule } from "./report.js";
import { validateThreadFilter } from "../data/filters.js";

export class CompareInputError extends Error {}

interface Side<T> { baseline: T; candidate: T; }
export interface CompareDto {
  schema_version: "compare.v1";
  compatibility: { status: "compatible"; checks: string[] };
  source: Side<ReportDto["source"]>;
  filters: Side<ReportDto["filters"]> & { window_comparison: "same" | "descriptive-different-creation-window" };
  totals: Side<ReportDto["totals"]>;
  rules: Array<{
    id: string; definition: string; threshold: string;
    baseline: { numerator: number | null; denominator: number | null; rate: number | null };
    candidate: { numerator: number | null; denominator: number | null; rate: number | null };
    rate_delta_pp: number | null; limitation?: string;
  }>;
  tools: Array<{
    label: string; outerName: string; effectiveName: string;
    baseline: { calls: number | null; errors: number | null; errorAffectedThreads: number | null; pairedKnownResults: number | null; errorRate: number | null };
    candidate: { calls: number | null; errors: number | null; errorAffectedThreads: number | null; pairedKnownResults: number | null; errorRate: number | null };
    error_rate_delta_pp: number | null;
  }>;
  result_bytes: Side<ReportDto["result_bytes"]> & { p50_delta: number | null; p95_delta: number | null };
  limitations: string[];
}

function object(value: unknown, path: string): Record<string, any> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new CompareInputError(path + " must be an object");
  return value as Record<string, any>;
}
function string(value: unknown, path: string): string {
  if (typeof value !== "string" || value.length === 0) throw new CompareInputError(path + " must be a non-empty string");
  return value;
}
function nonNegative(value: unknown, path: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) throw new CompareInputError(path + " must be a finite non-negative number");
  return value;
}
function nullableNonNegative(value: unknown, path: string): number | null {
  return value === null ? null : nonNegative(value, path);
}
function rate(value: unknown, path: string): number | null {
  const result = nullableNonNegative(value, path);
  if (result !== null && result > 1) throw new CompareInputError(path + " must be between 0 and 1");
  return result;
}
function readReport(path: string): ReportDto {
  let parsed: unknown;
  try { parsed = JSON.parse(readFileSync(path, "utf8")); }
  catch (error) { throw new CompareInputError("unable to read report " + path + ": " + (error instanceof Error ? error.message : String(error))); }
  const report = object(parsed, "report") as ReportDto;
  if (report.schema_version !== "report.v1") throw new CompareInputError("report.schema_version must be report.v1");
  const source = object(report.source, "report.source");
  string(source.fingerprint, "report.source.fingerprint"); string(source.normalizer, "report.source.normalizer"); string(source.metrics, "report.source.metrics");
  const filters = object(report.filters, "report.filters");
  if (!["roots", "children", "all"].includes(filters.scope)) throw new CompareInputError("report.filters.scope is invalid");
  if (typeof filters.includeHidden !== "boolean") throw new CompareInputError("report.filters.includeHidden must be boolean");
  for (const key of ["since", "until"]) {
    if (filters[key] !== null && typeof filters[key] !== "string") throw new CompareInputError("report.filters." + key + " must be string or null");
  }
  try {
    validateThreadFilter({
      scope: filters.scope,
      includeHidden: filters.includeHidden,
      since: filters.since ?? undefined,
      until: filters.until ?? undefined,
    });
  } catch (error) {
    throw new CompareInputError(error instanceof Error ? error.message : String(error));
  }
  const totals = object(report.totals, "report.totals");
  for (const key of ["threads", "messages", "calls", "pairedResults", "pairedKnownResults", "missingResults", "orphanResults", "duplicateCallIds", "duplicateResultIds", "unknownErrorResults", "explicitErrors", "parseIssues"]) nonNegative(totals[key], "report.totals." + key);
  if (totals.pairedKnownResults > totals.pairedResults || totals.pairedResults > totals.calls || totals.explicitErrors > totals.pairedKnownResults || totals.unknownErrorResults > totals.pairedResults) throw new CompareInputError("report.totals pairing/error counts are inconsistent");
  const pairing = object(report.pairing, "report.pairing");
  for (const key of ["paired_results", "paired_known_results", "calls", "missing_results", "orphan_results", "duplicate_call_ids", "duplicate_result_ids"]) nonNegative(pairing[key], "report.pairing." + key);
  if (pairing.paired_known_results > pairing.paired_results || pairing.paired_results > pairing.calls) throw new CompareInputError("report.pairing counts are inconsistent");
  const matchingCounts: Array<[string, number, number]> = [
    ["calls", totals.calls, pairing.calls],
    ["pairedResults", totals.pairedResults, pairing.paired_results],
    ["pairedKnownResults", totals.pairedKnownResults, pairing.paired_known_results],
    ["missingResults", totals.missingResults, pairing.missing_results],
    ["orphanResults", totals.orphanResults, pairing.orphan_results],
    ["duplicateCallIds", totals.duplicateCallIds, pairing.duplicate_call_ids],
    ["duplicateResultIds", totals.duplicateResultIds, pairing.duplicate_result_ids],
  ];
  if (matchingCounts.some(([, total, paired]) => total !== paired)) throw new CompareInputError("report totals and pairing fields are inconsistent");
  const expectedPairingRate = pairing.calls === 0 ? null : pairing.paired_results / pairing.calls;
  const expectedKnownRate = pairing.paired_results === 0 ? null : pairing.paired_known_results / pairing.paired_results;
  if (pairing.pairing_rate !== expectedPairingRate || pairing.known_result_rate !== expectedKnownRate) throw new CompareInputError("report.pairing rates do not match their denominators");
  const bytes = object(report.result_bytes, "report.result_bytes");
  nonNegative(bytes.count, "report.result_bytes.count"); nonNegative(bytes.giantCount, "report.result_bytes.giantCount"); nullableNonNegative(bytes.p50, "report.result_bytes.p50"); nullableNonNegative(bytes.p95, "report.result_bytes.p95"); nonNegative(bytes.errorBytes, "report.result_bytes.errorBytes");
  if (!Array.isArray(report.rules)) throw new CompareInputError("report.rules must be an array");
  for (const [index, rule] of report.rules.entries()) {
    const current = object(rule, "report.rules[" + index + "]");
    string(current.id, "report.rules[" + index + "].id"); string(current.threshold, "report.rules[" + index + "].threshold");
    string(current.definition, "report.rules[" + index + "].definition");
    if (!["observation", "candidate", "quality"].includes(current.kind)) throw new CompareInputError("report.rules[" + index + "].kind is invalid");
    const numerator = nullableNonNegative(current.numerator, "report.rules[" + index + "].numerator"); const denominator = nullableNonNegative(current.denominator, "report.rules[" + index + "].denominator"); const actualRate = rate(current.rate, "report.rules[" + index + "].rate");
    if (numerator !== null && denominator !== null && numerator > denominator) throw new CompareInputError("report.rules[" + index + "] numerator exceeds denominator");
    const expectedRate = numerator === null || denominator === null || denominator === 0 ? null : numerator / denominator;
    if (actualRate !== expectedRate) throw new CompareInputError("report.rules[" + index + "] rate does not match its denominator");
    if (!Array.isArray(current.evidence_ids) || typeof current.next_verification !== "string") throw new CompareInputError("report.rules[" + index + "] evidence or verification is malformed");
  }
  if (!Array.isArray(report.all_tools)) throw new CompareInputError("report.all_tools must be an array");
  const labels = new Set<string>();
  for (const [index, tool] of report.all_tools.entries()) {
    const current = object(tool, "report.all_tools[" + index + "]");
    string(current.label, "report.all_tools[" + index + "].label");
    string(current.outerName, "report.all_tools[" + index + "].outerName"); string(current.effectiveName, "report.all_tools[" + index + "].effectiveName");
    if (labels.has(current.label)) throw new CompareInputError("duplicate report.all_tools label: " + current.label);
    labels.add(current.label);
    for (const key of ["calls", "pairedResults", "pairedKnownResults", "errors", "unknownErrors", "calledThreads", "errorAffectedThreads"]) nonNegative(current[key], "report.all_tools[" + index + "]." + key);
    if (current.pairedKnownResults > current.pairedResults || current.pairedResults > current.calls || current.errors > current.pairedKnownResults || current.unknownErrors > current.pairedResults) throw new CompareInputError("report.all_tools[" + index + "] counts are inconsistent");
    const actualRate = rate(current.errorRate, "report.all_tools[" + index + "].errorRate");
    const expectedRate = current.pairedKnownResults === 0 ? null : current.errors / current.pairedKnownResults;
    if (actualRate !== expectedRate) throw new CompareInputError("report.all_tools[" + index + "] errorRate does not match its denominator");
  }
  const ruleIds = new Set<string>();
  for (const rule of report.rules) { if (ruleIds.has(rule.id)) throw new CompareInputError("duplicate report rule: " + rule.id); ruleIds.add(rule.id); }
  return report;
}
function delta(candidate: number | null, baseline: number | null): number | null {
  return candidate === null || baseline === null ? null : (candidate - baseline) * 100;
}
function ruleMap(report: ReportDto): Map<string, ReportRule> {
  return new Map(report.rules.map((rule) => [rule.id, rule]));
}
function metricSide(tool: any): { calls: number | null; errors: number | null; errorAffectedThreads: number | null; pairedKnownResults: number | null; errorRate: number | null } {
  if (!tool) return { calls: null, errors: null, errorAffectedThreads: null, pairedKnownResults: null, errorRate: null };
  return { calls: tool.calls, errors: tool.errors, errorAffectedThreads: tool.errorAffectedThreads, pairedKnownResults: tool.pairedKnownResults, errorRate: tool.errorRate };
}

export function compareReports(baseline: ReportDto, candidate: ReportDto): CompareDto {
  if (baseline.schema_version !== candidate.schema_version) throw new CompareInputError("schema versions are incompatible");
  if (baseline.source.normalizer !== candidate.source.normalizer) throw new CompareInputError("normalizer versions are incompatible");
  if (baseline.source.metrics !== candidate.source.metrics) throw new CompareInputError("metrics versions are incompatible");
  if (baseline.filters.scope !== candidate.filters.scope || baseline.filters.includeHidden !== candidate.filters.includeHidden) throw new CompareInputError("scope or includeHidden filters are incompatible");
  const baselineRules = ruleMap(baseline); const candidateRules = ruleMap(candidate);
  const rules: CompareDto["rules"] = [];
  for (const id of new Set([...baselineRules.keys(), ...candidateRules.keys()])) {
    const left = baselineRules.get(id); const right = candidateRules.get(id);
    if (left && right && (left.threshold !== right.threshold || left.definition !== right.definition || left.kind !== right.kind)) throw new CompareInputError("rule " + id + " definition, threshold, or kind is incompatible");
    const b = { numerator: left?.numerator ?? null, denominator: left?.denominator ?? null, rate: left?.rate ?? null };
    const c = { numerator: right?.numerator ?? null, denominator: right?.denominator ?? null, rate: right?.rate ?? null };
    rules.push({ id, definition: left?.definition ?? right?.definition ?? "unavailable", threshold: left?.threshold ?? right?.threshold ?? "unavailable", baseline: b, candidate: c, rate_delta_pp: delta(c.rate, b.rate), ...(!left || !right ? { limitation: "rule absent on one side; value remains null rather than being treated as zero" } : {}) });
  }
  const baselineTools = new Map(baseline.all_tools.map((tool) => [tool.label, tool]));
  const candidateTools = new Map(candidate.all_tools.map((tool) => [tool.label, tool]));
  const tools: CompareDto["tools"] = [];
  for (const label of new Set([...baselineTools.keys(), ...candidateTools.keys()])) {
    const left = baselineTools.get(label); const right = candidateTools.get(label);
    tools.push({ label, outerName: left?.outerName ?? right?.outerName ?? label, effectiveName: left?.effectiveName ?? right?.effectiveName ?? label, baseline: metricSide(left), candidate: metricSide(right), error_rate_delta_pp: delta(right?.errorRate ?? null, left?.errorRate ?? null) });
  }
  const differentWindow = JSON.stringify({ since: baseline.filters.since, until: baseline.filters.until }) !== JSON.stringify({ since: candidate.filters.since, until: candidate.filters.until });
  return {
    schema_version: "compare.v1",
    compatibility: { status: "compatible", checks: ["schema_version", "normalizer", "metrics", "scope", "includeHidden", "rule definitions and thresholds"] },
    source: { baseline: baseline.source, candidate: candidate.source },
    filters: { baseline: baseline.filters, candidate: candidate.filters, window_comparison: differentWindow ? "descriptive-different-creation-window" : "same" },
    totals: { baseline: baseline.totals, candidate: candidate.totals }, rules, tools,
    result_bytes: { baseline: baseline.result_bytes, candidate: candidate.result_bytes, p50_delta: baseline.result_bytes.p50 === null || candidate.result_bytes.p50 === null ? null : candidate.result_bytes.p50 - baseline.result_bytes.p50, p95_delta: baseline.result_bytes.p95 === null || candidate.result_bytes.p95 === null ? null : candidate.result_bytes.p95 - baseline.result_bytes.p95 },
    limitations: [...(differentWindow ? ["Creation windows differ; differences describe two task populations and do not establish a causal improvement."] : []), "Different thread composition, message format, model, or data quality can explain observed differences.", "Missing rules and missing tools remain null; absence is not interpreted as zero errors or zero calls."],
  };
}

export function compareReportFiles(baselinePath: string, candidatePath: string): CompareDto {
  return compareReports(readReport(baselinePath), readReport(candidatePath));
}

export function renderCompareMarkdown(result: CompareDto): string {
  const cell = (value: unknown): string => String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("|", "\\|").replaceAll("\n", " ");
  const rules = result.rules.map((rule) => "| " + cell(rule.id) + " | " + cell(rule.threshold) + " | " + rule.baseline.numerator + "/" + rule.baseline.denominator + " | " + rule.candidate.numerator + "/" + rule.candidate.denominator + " | " + (rule.baseline.rate === null ? "null" : (rule.baseline.rate * 100).toFixed(2) + "%") + " | " + (rule.candidate.rate === null ? "null" : (rule.candidate.rate * 100).toFixed(2) + "%") + " | " + (rule.rate_delta_pp === null ? "null" : rule.rate_delta_pp.toFixed(2) + "pp") + " |").join("\n") || "| - | - | -/- | -/- | null | null | null |";
  const tools = result.tools.map((tool) => "| " + cell(tool.label) + " | " + tool.baseline.errors + "/" + (tool.baseline.pairedKnownResults ?? "null") + " | " + tool.candidate.errors + "/" + (tool.candidate.pairedKnownResults ?? "null") + " | " + (tool.baseline.errorRate === null ? "null" : (tool.baseline.errorRate * 100).toFixed(2) + "%") + " | " + (tool.candidate.errorRate === null ? "null" : (tool.candidate.errorRate * 100).toFixed(2) + "%") + " | " + (tool.error_rate_delta_pp === null ? "null" : tool.error_rate_delta_pp.toFixed(2) + "pp") + " |").join("\n") || "| - | -/- | -/- | null | null | null |";
  return ["# Comparison report", "", "- compatibility: " + result.compatibility.status, "- window comparison: " + result.filters.window_comparison, "- baseline source: " + cell(result.source.baseline.fingerprint), "- candidate source: " + cell(result.source.candidate.fingerprint), "- baseline filters: scope=" + result.filters.baseline.scope + ", include_hidden=" + result.filters.baseline.includeHidden + ", since=" + cell(result.filters.baseline.since ?? "-") + ", until=" + cell(result.filters.baseline.until ?? "-"), "- candidate filters: scope=" + result.filters.candidate.scope + ", include_hidden=" + result.filters.candidate.includeHidden + ", since=" + cell(result.filters.candidate.since ?? "-") + ", until=" + cell(result.filters.candidate.until ?? "-"), "", "## Samples", "", "- baseline threads/messages/calls: " + result.totals.baseline.threads + "/" + result.totals.baseline.messages + "/" + result.totals.baseline.calls, "- candidate threads/messages/calls: " + result.totals.candidate.threads + "/" + result.totals.candidate.messages + "/" + result.totals.candidate.calls, "- paired known results baseline/candidate: " + result.totals.baseline.pairedKnownResults + "/" + result.totals.candidate.pairedKnownResults, "", "## Rule rates", "", "| rule | threshold | baseline n/d | candidate n/d | baseline rate | candidate rate | delta |", "| --- | --- | ---: | ---: | ---: | ---: | ---: |", rules, "", "## Tool error rates", "", "| tool | baseline errors/known | candidate errors/known | baseline rate | candidate rate | delta |", "| --- | ---: | ---: | ---: | ---: | ---: |", tools, "", "## Limitations", "", ...result.limitations.map((item) => "- " + cell(item)), ""].join("\n");
}

export function writeCompareReports(result: CompareDto, outDir: string): void {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(outDir + "/compare.json", JSON.stringify(result, null, 2));
  writeFileSync(outDir + "/compare.md", renderCompareMarkdown(result));
}
