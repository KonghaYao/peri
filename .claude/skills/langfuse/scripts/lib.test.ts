import { describe, expect, test } from "bun:test";
import { auditObservationTree, clampTraceLimit, detectAnomalies, fmtLatency, parseFilterArgs, splitGenerationAgentSegments, summarizeErrors, summarizeLatency, summarizeTraceMetrics } from "./lib.ts";

describe("Langfuse report pure logic", () => {
  test("trace 查询 limit 被限制在 API 上限 100", () => {
    expect(clampTraceLimit(500)).toBe(100);
    expect(parseFilterArgs(["--limit", "500"]).limit).toBe(100);
    expect(parseFilterArgs(["500"]).limit).toBe(100);
  });

  test("agent-run 时长优先于缺失的 trace latency", () => {
    expect(summarizeLatency([{ type: "AGENT", name: "agent-run", startTime: "2026-01-01T00:00:00.000Z", endTime: "2026-01-01T00:00:12.500Z" }])).toEqual({ seconds: 12.5, source: "agent-run" });
  });

  test("缺少 agent-run 时使用 observation 时间包络，完全缺失时不可用", () => {
    expect(summarizeLatency([{ startTime: "2026-01-01T00:00:03.000Z", endTime: "2026-01-01T00:00:05.000Z" }, { startTime: "2026-01-01T00:00:01.000Z", endTime: "2026-01-01T00:00:08.000Z" }])).toEqual({ seconds: 7, source: "observations" });
    expect(summarizeLatency([{ startTime: "invalid" }])).toEqual({ seconds: null, source: "unavailable" });
    expect(fmtLatency(summarizeLatency([]))).toBe("N/A");
  });

  test("长 loop 输出统一可解释计数与真实时长", () => {
    const observations = [
      { type: "AGENT", name: "agent-run", startTime: "2026-01-01T00:00:00.000Z", endTime: "2026-01-01T00:02:10.000Z" },
      ...Array.from({ length: 11 }, () => ({ type: "GENERATION", usageDetails: { input: 3000, cache_read_input_tokens: 1000 } })),
      ...Array.from({ length: 12 }, () => ({ type: "TOOL" })),
    ];
    const loop = detectAnomalies(summarizeTraceMetrics(observations)).find((item) => item.type === "loop");
    expect(loop).toMatchObject({ severity: "high" });
    expect(loop?.description).toContain("LLM=11");
    expect(loop?.description).toContain("Tools=12");
    expect(loop?.description).toContain("真实耗时=2m10s");
  });

  test("错误分类不会保留 prompt、工具结果或错误正文", () => {
    const secret = "sentinel-secret";
    const summary = summarizeErrors([
      { type: "GENERATION", level: "ERROR", input: secret, output: secret, statusMessage: secret },
      { type: "TOOL", level: "ERROR", input: secret, output: secret },
      { type: "SPAN", status: "CANCELLED", metadata: { prompt: secret } },
    ]);
    expect(summary).toEqual({ hasError: true, categories: ["cancelled_or_user_aborted", "provider_or_stream_failure", "tool_failure"], failedLlmCalls: 1, failedToolCalls: 1 });
    expect(JSON.stringify(summary)).not.toContain(secret);
  });

  test("稳定 error_class 映射为报告分类，不退化为 unknown", () => {
    const summary = summarizeErrors([
      { type: "SPAN", level: "ERROR", metadata: { error_class: "llm_failure" } },
      { type: "SPAN", level: "ERROR", metadata: { error_class: "tool_failure" } },
      { type: "SPAN", level: "ERROR", metadata: { error_class: "timeout" } },
      { type: "SPAN", level: "ERROR", metadata: { error_class: "rate_limit" } },
    ]);
    expect(summary).toEqual({ hasError: true, categories: ["provider_or_stream_failure", "rate_limit", "timeout", "tool_failure"], failedLlmCalls: 0, failedToolCalls: 0 });
  });

  test("主 agent、subagent、主 agent 被拆为连续的独立段", () => {
    const observations = [
      { id: "main", type: "AGENT", name: "agent-run" }, { id: "child", type: "AGENT", name: "subagent-review" },
      { id: "g1", type: "GENERATION", parentObservationId: "main", startTime: "2026-01-01T00:00:01Z" },
      { id: "g2", type: "GENERATION", parentObservationId: "child", startTime: "2026-01-01T00:00:02Z" },
      { id: "g3", type: "GENERATION", parentObservationId: "main", startTime: "2026-01-01T00:00:03Z" },
    ];
    expect(splitGenerationAgentSegments(observations).map((segment) => [segment.agentObservationId, segment.generationIds])).toEqual([["main", ["g1"]], ["child", ["g2"]], ["main", ["g3"]]]);
  });

  test("observation tree audit 接受 trace root 并报告缺失 parent、重复 ID 与环", () => {
    const audit = auditObservationTree([
      { id: "root", parentObservationId: "trace-1" },
      { id: "child", parentObservationId: "root" },
      { id: "orphan", parentObservationId: "missing" },
      { id: "duplicate", parentObservationId: "trace-1" },
      { id: "duplicate", parentObservationId: "trace-1" },
      { id: "cycle-a", parentObservationId: "cycle-b" },
      { id: "cycle-b", parentObservationId: "cycle-a" },
    ], "trace-1");

    expect(audit.duplicateIds).toEqual(["duplicate"]);
    expect(audit.missingParents).toEqual([{ id: "orphan", parentObservationId: "missing" }]);
    expect(audit.cycles).toEqual([["cycle-a", "cycle-b", "cycle-a"]]);
  });
});
