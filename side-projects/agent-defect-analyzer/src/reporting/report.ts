import { mkdirSync, writeFileSync } from "node:fs";
import { analyzeDatabase, type AnalysisReport, type AnalysisOptions, type Candidate } from "../analysis/metrics.js";

export interface ReportRule {
  id: string;
  kind: "observation" | "candidate" | "quality";
  definition: string;
  threshold: string;
  numerator: number | null;
  denominator: number | null;
  rate: number | null;
  evidence_ids: Array<{ threadId: string; messageId: string; callId: string }>;
  next_verification: string;
}

export interface ReportDto {
  schema_version: "report.v1";
  source: { fingerprint: string; normalizer: string; metrics: string };
  filters: AnalysisReport["filters"];
  totals: AnalysisReport["totals"];
  pairing: {
    paired_results: number; paired_known_results: number; calls: number;
    missing_results: number; orphan_results: number; duplicate_call_ids: number;
    duplicate_result_ids: number; pairing_rate: number | null; known_result_rate: number | null;
  };
  top_tool_errors: Array<AnalysisReport["tools"][string]>;
  all_tools: Array<AnalysisReport["tools"][string]>;
  result_bytes: AnalysisReport["resultBytes"];
  candidates: Candidate[];
  rules: ReportRule[];
  capabilities: Record<string, { status: "available" | "unavailable"; reason: string }>;
}

function ratio(numerator: number, denominator: number): number | null {
  return denominator === 0 ? null : numerator / denominator;
}

function thresholdFor(ruleId: string): string {
  switch (ruleId) {
    case "repeated-call": return "same canonical call and arguments occur at least 3 times consecutively within a thread";
    case "explicit-failure": return "the same labeled tool produces at least 2 consecutive explicit errors within a thread";
    case "large-tool-output": return "UTF-8 result content is at least 100000 bytes";
    default: return "defined by the metrics analyzer; inspect evidence before interpretation";
  }
}

function definitionFor(ruleId: string): string {
  switch (ruleId) {
    case "repeated-call": return "count of consecutive-call runs at the first point each run reaches 3 calls; a longer run contributes one event";
    case "explicit-failure": return "count of explicit-failure runs at the first point each run reaches 2 errors; a longer run contributes one event";
    case "large-tool-output": return "count of normalized tool results at or above the UTF-8 byte threshold";
    case "pairing-quality": return "paired normalized tool results divided by normalized tool calls";
    case "known-result-state": return "paired results with explicit success or error divided by paired results";
    default: return "defined by the metrics analyzer";
  }
}

function candidateRule(candidate: Candidate): ReportRule {
  return {
    id: candidate.ruleId, kind: candidate.evidenceKind, threshold: thresholdFor(candidate.ruleId),
    definition: definitionFor(candidate.ruleId),
    numerator: candidate.counts, denominator: candidate.denominator,
    rate: ratio(candidate.counts, candidate.denominator),
    evidence_ids: candidate.evidence.entries, next_verification: candidate.nextVerification,
  };
}

function buildReport(analysis: AnalysisReport): ReportDto {
  const allTools = Object.values(analysis.tools).sort((left, right) =>
    right.errors - left.errors || right.errorAffectedThreads - left.errorAffectedThreads || left.label.localeCompare(right.label));
  const tools = allTools.filter((tool) => tool.errors > 0);
  const pairing: ReportDto["pairing"] = {
    paired_results: analysis.totals.pairedResults, paired_known_results: analysis.totals.pairedKnownResults,
    calls: analysis.totals.calls, missing_results: analysis.totals.missingResults,
    orphan_results: analysis.totals.orphanResults, duplicate_call_ids: analysis.totals.duplicateCallIds,
    duplicate_result_ids: analysis.totals.duplicateResultIds,
    pairing_rate: ratio(analysis.totals.pairedResults, analysis.totals.calls),
    known_result_rate: ratio(analysis.totals.pairedKnownResults, analysis.totals.pairedResults),
  };
  const qualityRules: ReportRule[] = [
    {
      id: "pairing-quality", kind: "quality",
      definition: definitionFor("pairing-quality"),
      threshold: "paired results / normalized tool calls; known result state / paired results",
      numerator: pairing.paired_results, denominator: pairing.calls, rate: pairing.pairing_rate,
      evidence_ids: [], next_verification: "Inspect missing and orphan result evidence before using error rates for behavior claims.",
    },
    {
      id: "known-result-state", kind: "quality",
      definition: definitionFor("known-result-state"),
      threshold: "paired results with explicit success or error state / paired results",
      numerator: pairing.paired_known_results, denominator: pairing.paired_results, rate: pairing.known_result_rate,
      evidence_ids: [], next_verification: "Review unknown result states and source serialization before treating error rates as complete.",
    },
  ];
  const candidateByRule = new Map(analysis.candidates.map((candidate) => [candidate.ruleId, candidate]));
  const fixedRules: ReportRule[] = [
    ...qualityRules,
    ...(["repeated-call", "explicit-failure", "large-tool-output"] as const).map((ruleId) => {
      const candidate = candidateByRule.get(ruleId);
      const denominator = ruleId === "repeated-call" ? analysis.totals.calls : ruleId === "explicit-failure" ? analysis.totals.pairedKnownResults : analysis.resultBytes.count;
      const numerator = candidate?.counts ?? 0;
      const kind: ReportRule["kind"] = ruleId === "large-tool-output" ? "observation" : "candidate";
      return { id: ruleId, kind, definition: definitionFor(ruleId), threshold: thresholdFor(ruleId), numerator, denominator, rate: ratio(numerator, denominator), evidence_ids: candidate?.evidence.entries ?? [], next_verification: candidate?.nextVerification ?? "No qualifying observation in this report; verify the denominator and inspect a seeded sample before concluding absence." } as ReportRule;
    }),
  ];
  return {
    schema_version: "report.v1",
    source: { fingerprint: analysis.source.fingerprint, normalizer: analysis.version.normalizer, metrics: analysis.version.metrics },
    filters: analysis.filters, totals: analysis.totals, pairing, top_tool_errors: tools, all_tools: allTools,
    result_bytes: analysis.resultBytes, candidates: analysis.candidates,
    rules: fixedRules,
    capabilities: {
      tool_errors: { status: "available", reason: "paired normalized tool results with explicit error state" },
      repeated_calls: { status: "available", reason: "same labeled tool and stable canonical arguments within a thread" },
      result_bytes: { status: "available", reason: "UTF-8 byte counts from normalized tool results" },
      token_usage: { status: "unavailable", reason: "no stable token source in the analysis contract" },
      latency: { status: "unavailable", reason: "persisted event timestamps are not a stable execution duration source" },
      completion: { status: "unavailable", reason: "completion requires an explicit outcome label or human evidence" },
      execution_evidence: { status: "available", reason: "typed execution metadata is counted separately from is_error tool-call errors; missing legacy metadata remains unknown" },
    },
  };
}

function cell(value: unknown): string {
  return String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
    .replaceAll("|", "\\|").replaceAll("\n", " ").replaceAll("\r", " ");
}

function percent(value: number | null): string {
  return value === null ? "unavailable" : (value * 100).toFixed(2) + "%";
}

export function renderReportMarkdown(report: ReportDto): string {
  const topErrors = report.top_tool_errors.length === 0
    ? "| - | 0 | 0 | 0 | unavailable |"
    : report.top_tool_errors.map((tool) =>
      "| " + cell(tool.label) + " | " + tool.errors + " | " + tool.errorAffectedThreads + " | " +
      tool.pairedKnownResults + " | " + cell(percent(tool.errorRate)) + " |").join("\n");
  const rules = report.rules.map((rule) =>
    "| " + cell(rule.id) + " | " + cell(rule.kind) + " | " + cell(rule.threshold) + " | " +
    (rule.numerator ?? "-") + " | " + (rule.denominator ?? "-") + " | " + cell(percent(rule.rate)) +
    " | " + cell(rule.next_verification) + " |").join("\n");
  const candidates = report.candidates.length === 0
    ? "| - | 0 | 0 | - |"
    : report.candidates.map((candidate) =>
      "| " + cell(candidate.ruleId) + " | " + candidate.counts + " | " + candidate.denominator + " | " +
      (candidate.evidence.entries.length === 0 ? "-" : candidate.evidence.entries.map((entry) => "[report.json](report.json) :: " + entry.threadId + "/" + entry.messageId + "/" + entry.callId).join("; ")) + " | " + cell(candidate.nextVerification) + " |").join("\n");
  return [
    "# Agent behavior report", "",
    "- schema: " + report.schema_version,
    "- source fingerprint: " + cell(report.source.fingerprint),
    "- normalizer: " + cell(report.source.normalizer) + "; metrics: " + cell(report.source.metrics),
    "- filters: scope=" + cell(report.filters.scope) + ", include_hidden=" + report.filters.includeHidden +
      ", since=" + cell(report.filters.since ?? "-") + ", until=" + cell(report.filters.until ?? "-"),
    "- 默认输出只包含统计、规则和 bounded evidence refs，不包含原始消息、参数或路径。", "",
    "## Totals", "",
    "- threads=" + report.totals.threads + ", messages=" + report.totals.messages + ", calls=" + report.totals.calls,
    "- explicit_errors=" + report.totals.explicitErrors + ", unknown_error_results=" +
      report.totals.unknownErrorResults + ", parse_issues=" + report.totals.parseIssues, "",
    ...(report.totals.execution ? [
      "## Execution evidence", "",
      "- typed=" + report.totals.execution.typedCount + "/" + report.totals.execution.resultCount +
        " (" + percent(report.totals.execution.coverage) + "), known statuses=" + report.totals.execution.knownCount,
      "- statuses=" + JSON.stringify(report.totals.execution.statusCounts) + ", output_refs=" +
        report.totals.execution.outputRefCount + ", truncated_outputs=" + report.totals.execution.outputTruncatedCount, "",
    ] : []),
    "## Top tool errors", "",
    "| tool | errors | affected threads | known result denominator | error rate |",
    "| --- | ---: | ---: | ---: | ---: |", topErrors, "",
    "## Pairing", "",
    "- paired=" + report.pairing.paired_results + "/" + report.pairing.calls + " (" + percent(report.pairing.pairing_rate) + ")",
    "- known state=" + report.pairing.paired_known_results + "/" + report.pairing.paired_results +
      " (" + percent(report.pairing.known_result_rate) + ")",
    "- missing=" + report.pairing.missing_results + ", orphan=" + report.pairing.orphan_results +
      ", duplicate calls=" + report.pairing.duplicate_call_ids + ", duplicate results=" + report.pairing.duplicate_result_ids, "",
    "## Result bytes", "",
    "- count=" + report.result_bytes.count + ", p50=" + (report.result_bytes.p50 ?? "unavailable") +
      ", p95=" + (report.result_bytes.p95 ?? "unavailable") + ", giant=" + report.result_bytes.giantCount +
      ", error_bytes=" + report.result_bytes.errorBytes, "",
    "## Rules", "",
    "| rule | kind | threshold | numerator | denominator | rate | next verification |",
    "| --- | --- | --- | ---: | ---: | ---: | --- |", rules, "",
    "## Candidates", "",
    "| rule | count | denominator | evidence refs | next verification |",
    "| --- | ---: | ---: | --- | --- |", candidates, "",
    "## Capabilities", "",
    ...Object.entries(report.capabilities).map(([name, capability]) =>
      "- " + cell(name) + ": **" + cell(capability.status) + "** — " + cell(capability.reason)),
    "",
  ].join("\n");
}

export function reportDatabase(dbPath: string, options: AnalysisOptions = {}): ReportDto {
  return buildReport(analyzeDatabase(dbPath, options));
}

export function writeReportReports(report: ReportDto, outDir: string): void {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(outDir + "/report.json", JSON.stringify(report, null, 2));
  writeFileSync(outDir + "/report.md", renderReportMarkdown(report));
}
