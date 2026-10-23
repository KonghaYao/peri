/** Stable JSON and Markdown projection for the shell waiting analysis. */
import { mkdirSync, writeFileSync } from "node:fs";
import type { ShellWaitingReport } from "../analysis/waiting.js";
import { renderWaitingHtml } from "./waiting_html.js";

export interface WaitingDto {
  schema_version: "waiting.v1";
  source: { fingerprint: string; normalizer: string; metrics: string };
  filters: { scope: string; include_hidden: boolean; since: string | null; until: string | null };
  totals: {
    threads: number; messages: number; tool_calls: number; command_calls: number; command_threads: number;
    duplicate_call_ids: number; duplicate_result_ids: number; missing_sleep_results: number; parse_issues: number;
  };
  tools: Array<{ name: string; calls: number; command_calls: number; sleep_calls: number; sleep_share: number | null }>;
  sleep: {
    calls: number; share_of_command_calls: number | null; threads_with_sleep: number; thread_share: number | null;
    total_sleep_seconds: number;
    call_sleep_seconds: { p50: number | null; p90: number | null; max: number | null; known_samples: number };
    thread_sleep_seconds: { p50: number | null; p90: number | null; max: number | null; known_samples: number };
    background_sleep_calls: number; sleep_exceeding_timeout_calls: number;
    by_shape: Record<string, number>;
    by_wait_kind: Record<string, number>;
    by_duration_bucket: Record<string, number>;
    by_execution_status: Record<string, number>;
    by_is_error: Record<string, number>;
    by_thread_type: Record<string, { command_calls: number; sleep_calls: number; threads_with_command: number; threads_with_sleep: number }>;
    by_month: Record<string, { command_calls: number; sleep_calls: number; sleep_share: number | null }>;
    by_week: Record<string, { command_calls: number; sleep_calls: number; sleep_share: number | null }>;
    thread_sleep_histogram: Record<string, number>;
    top_threads: Array<{ thread_id: string; thread_kind: string; created_month: string; command_calls: number; sleep_calls: number; sleep_share: number | null; sleep_seconds: number }>;
    examples: Array<{ thread_id: string; message_id: string; call_id: string; tool: string; shape: string; wait_kind: string; seconds: number | null; variable_sleep: boolean; background: boolean; timeout_ms: number | null; status: string; is_error: boolean | null }>;
  };
  sensitivity: { broad_text_match_calls: number; wrapper_embedded_sleep_calls: number; note: string };
  parse_issues: Record<string, number>;
  capabilities: Record<string, { status: string; reason: string }>;
}

function mapRecord<T>(record: Record<string, T>, mapper: (value: T, key: string) => unknown): Record<string, unknown> {
  return Object.fromEntries(Object.entries(record).map(([key, value]) => [key, mapper(value, key)]));
}

export function buildWaitingDto(report: ShellWaitingReport): WaitingDto {
  return {
    schema_version: "waiting.v1",
    source: { fingerprint: report.source.fingerprint, normalizer: report.version.normalizer, metrics: report.version.metrics },
    filters: {
      scope: report.filters.scope,
      include_hidden: report.filters.includeHidden,
      since: report.filters.since,
      until: report.filters.until,
    },
    totals: {
      threads: report.totals.threads,
      messages: report.totals.messages,
      tool_calls: report.totals.toolCalls,
      command_calls: report.totals.commandCalls,
      command_threads: report.totals.commandThreads,
      duplicate_call_ids: report.totals.duplicateCallIds,
      duplicate_result_ids: report.totals.duplicateResultIds,
      missing_sleep_results: report.totals.missingSleepResults,
      parse_issues: report.totals.parseIssues,
    },
    tools: report.tools.map((tool) => ({
      name: tool.name,
      calls: tool.calls,
      command_calls: tool.commandCalls,
      sleep_calls: tool.sleepCalls,
      sleep_share: tool.sleepShare,
    })),
    sleep: {
      calls: report.sleep.calls,
      share_of_command_calls: report.sleep.shareOfCommandCalls,
      threads_with_sleep: report.sleep.threadsWithSleep,
      thread_share: report.sleep.threadShare,
      total_sleep_seconds: report.sleep.totalSleepSeconds,
      call_sleep_seconds: {
        p50: report.sleep.callSleepSeconds.p50,
        p90: report.sleep.callSleepSeconds.p90,
        max: report.sleep.callSleepSeconds.max,
        known_samples: report.sleep.callSleepSeconds.knownSamples,
      },
      thread_sleep_seconds: {
        p50: report.sleep.threadSleepSeconds.p50,
        p90: report.sleep.threadSleepSeconds.p90,
        max: report.sleep.threadSleepSeconds.max,
        known_samples: report.sleep.threadSleepSeconds.knownSamples,
      },
      background_sleep_calls: report.sleep.backgroundSleepCalls,
      sleep_exceeding_timeout_calls: report.sleep.sleepExceedingTimeoutCalls,
      by_shape: report.sleep.byShape,
      by_wait_kind: report.sleep.byWaitKind,
      by_duration_bucket: report.sleep.byDurationBucket,
      by_execution_status: report.sleep.byExecutionStatus,
      by_is_error: report.sleep.byIsError,
      by_thread_type: mapRecord(report.sleep.byThreadType, (value) => ({
        command_calls: value.commandCalls,
        sleep_calls: value.sleepCalls,
        threads_with_command: value.threadsWithCommand,
        threads_with_sleep: value.threadsWithSleep,
      })) as WaitingDto["sleep"]["by_thread_type"],
      by_month: mapRecord(report.sleep.byMonth, (value) => ({
        command_calls: value.commandCalls,
        sleep_calls: value.sleepCalls,
        sleep_share: value.sleepShare,
      })) as WaitingDto["sleep"]["by_month"],
      by_week: mapRecord(report.sleep.byWeek, (value) => ({
        command_calls: value.commandCalls,
        sleep_calls: value.sleepCalls,
        sleep_share: value.sleepShare,
      })) as WaitingDto["sleep"]["by_week"],
      thread_sleep_histogram: report.sleep.threadSleepHistogram,
      top_threads: report.sleep.topThreads.map((thread) => ({
        thread_id: thread.threadId,
        thread_kind: thread.threadKind,
        created_month: thread.createdMonth,
        command_calls: thread.commandCalls,
        sleep_calls: thread.sleepCalls,
        sleep_share: thread.sleepShare,
        sleep_seconds: thread.sleepSeconds,
      })),
      examples: report.sleep.examples.map((entry) => ({
        thread_id: entry.threadId,
        message_id: entry.messageId,
        call_id: entry.callId,
        tool: entry.tool,
        shape: entry.shape,
        wait_kind: entry.waitKind,
        seconds: entry.seconds,
        variable_sleep: entry.variableSleep,
        background: entry.background,
        timeout_ms: entry.timeoutMs,
        status: entry.status,
        is_error: entry.isError,
      })),
    },
    sensitivity: {
      broad_text_match_calls: report.sensitivity.broadTextMatchCalls,
      wrapper_embedded_sleep_calls: report.sensitivity.wrapperEmbeddedSleepCalls,
      note: report.sensitivity.note,
    },
    parse_issues: report.parseIssues,
    capabilities: report.capabilities,
  };
}

function cell(value: unknown): string {
  return String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
    .replaceAll("|", "\\|").replaceAll("\n", " ").replaceAll("\r", " ");
}

function percent(value: number | null): string {
  return value === null ? "unavailable" : (value * 100).toFixed(2) + "%";
}

function table(rows: Array<[string, string | number]>): string[] {
  return ["| key | value |", "| --- | ---: |", ...rows.map(([key, value]) => `| ${cell(key)} | ${cell(value)} |`)];
}

export function renderWaitingMarkdown(dto: WaitingDto): string {
  const sleep = dto.sleep;
  const monthRows = Object.entries(sleep.by_month).map(([month, value]) =>
    `| ${cell(month)} | ${value.command_calls} | ${value.sleep_calls} | ${cell(percent(value.sleep_share))} |`);
  const weekRows = Object.entries(sleep.by_week).map(([week, value]) =>
    `| ${cell(week)} | ${value.command_calls} | ${value.sleep_calls} | ${cell(percent(value.sleep_share))} |`);
  const toolRows = dto.tools.filter((tool) => tool.command_calls > 0).slice(0, 15).map((tool) =>
    `| ${cell(tool.name)} | ${tool.calls} | ${tool.command_calls} | ${tool.sleep_calls} | ${cell(percent(tool.sleep_share))} |`);
  const threadRows = sleep.top_threads.map((thread) =>
    `| ${cell(thread.thread_id)} | ${cell(thread.thread_kind)} | ${cell(thread.created_month)} | ${thread.command_calls} | ${thread.sleep_calls} | ${cell(percent(thread.sleep_share))} | ${thread.sleep_seconds} |`);
  const exampleRows = sleep.examples.map((entry) =>
    `| ${cell(entry.thread_id)} | ${cell(entry.message_id)} | ${cell(entry.call_id)} | ${cell(entry.tool)} | ${cell(entry.shape)} | ${cell(entry.wait_kind)} | ${entry.seconds ?? "unknown"} | ${cell(entry.status)} |`);
  return [
    "# Shell sleep waiting report", "",
    `- schema: ${dto.schema_version}`,
    `- source fingerprint: ${cell(dto.source.fingerprint)}`,
    `- normalizer: ${cell(dto.source.normalizer)}; metrics: ${cell(dto.source.metrics)}`,
    `- filters: scope=${cell(dto.filters.scope)}, include_hidden=${dto.filters.include_hidden}, since=${cell(dto.filters.since ?? "-")}, until=${cell(dto.filters.until ?? "-")}`,
    "- 默认输出只包含统计和 bounded evidence refs，不包含原始命令、参数或路径。", "",
    "## Totals", "",
    ...table([
      ["threads", dto.totals.threads],
      ["messages", dto.totals.messages],
      ["tool_calls", dto.totals.tool_calls],
      ["command_calls", dto.totals.command_calls],
      ["command_threads", dto.totals.command_threads],
      ["sleep_calls", sleep.calls],
      ["sleep_share", percent(sleep.share_of_command_calls)],
      ["threads_with_sleep", sleep.threads_with_sleep],
      ["thread_share", percent(sleep.thread_share)],
      ["missing_sleep_results", dto.totals.missing_sleep_results],
      ["duplicate_call_ids", dto.totals.duplicate_call_ids],
      ["duplicate_result_ids", dto.totals.duplicate_result_ids],
      ["parse_issues", dto.totals.parse_issues],
    ]), "",
    "## Sleep shape", "",
    "| shape | count |", "| --- | ---: |",
    ...Object.entries(sleep.by_shape).map(([key, value]) => `| ${cell(key)} | ${value} |`), "",
    "## Wait information", "",
    "| wait kind | count |", "| --- | ---: |",
    ...Object.entries(sleep.by_wait_kind).map(([key, value]) => `| ${cell(key)} | ${value} |`),
    "",
    "`pure` = only sleep; `noop_only` = sleep plus echo/printf/true/false/:/date; `other_command` = at least one other command (observation or action).", "",
    "## Duration bucket", "",
    "| bucket | count |", "| --- | ---: |",
    ...Object.entries(sleep.by_duration_bucket).map(([key, value]) => `| ${cell(key)} | ${value} |`), "",
    "## Execution status", "",
    "| status | count |", "| --- | ---: |",
    ...Object.entries(sleep.by_execution_status).map(([key, value]) => `| ${cell(key)} | ${value} |`),
    "",
    "- is_error: " + JSON.stringify(sleep.by_is_error),
    `- call sleep seconds: p50=${sleep.call_sleep_seconds.p50 ?? "unavailable"}, p90=${sleep.call_sleep_seconds.p90 ?? "unavailable"}, max=${sleep.call_sleep_seconds.max ?? "unavailable"}, known=${sleep.call_sleep_seconds.known_samples}`,
    `- thread total sleep seconds: p50=${sleep.thread_sleep_seconds.p50 ?? "unavailable"}, p90=${sleep.thread_sleep_seconds.p90 ?? "unavailable"}, max=${sleep.thread_sleep_seconds.max ?? "unavailable"}`,
    `- background sleep calls=${sleep.background_sleep_calls}; sleep longer than timeout=${sleep.sleep_exceeding_timeout_calls}`, "",
    "## Thread types", "",
    "| type | command calls | sleep calls | threads with command | threads with sleep |",
    "| --- | ---: | ---: | ---: | ---: |",
    ...Object.entries(sleep.by_thread_type).map(([key, value]) =>
      `| ${cell(key)} | ${value.command_calls} | ${value.sleep_calls} | ${value.threads_with_command} | ${value.threads_with_sleep} |`), "",
    "## By month", "",
    "| month | command calls | sleep calls | share |", "| --- | ---: | ---: | ---: |", ...monthRows, "",
    "## By week", "",
    "| week (UTC Monday) | command calls | sleep calls | share |", "| --- | ---: | ---: | ---: |", ...weekRows, "",
    "## Tools", "",
    "| tool | calls | command calls | sleep calls | share |", "| --- | ---: | ---: | ---: | ---: |",
    ...(toolRows.length === 0 ? ["| - | 0 | 0 | 0 | unavailable |"] : toolRows), "",
    "## Top sleep threads", "",
    "| thread | type | month | command calls | sleep calls | share | sleep seconds |",
    "| --- | --- | --- | ---: | ---: | ---: | ---: |",
    ...(threadRows.length === 0 ? ["| - | - | - | 0 | 0 | unavailable | 0 |"] : threadRows), "",
    "## Evidence examples", "",
    "| thread | message | call | tool | shape | wait kind | seconds | status |",
    "| --- | --- | --- | --- | --- | --- | ---: | --- |",
    ...(exampleRows.length === 0 ? ["| - | - | - | - | - | - | - | - |"] : exampleRows), "",
    "## Sensitivity", "",
    `- broad text match calls: ${dto.sensitivity.broad_text_match_calls}`,
    `- wrapper embedded sleep calls: ${dto.sensitivity.wrapper_embedded_sleep_calls}`,
    `- ${cell(dto.sensitivity.note)}`, "",
    "## Parse issues", "",
    ...(Object.keys(dto.parse_issues).length === 0
      ? ["- none"]
      : Object.entries(dto.parse_issues).map(([key, value]) => `- ${cell(key)}: ${value}`)), "",
    "## Capabilities", "",
    ...Object.entries(dto.capabilities).map(([name, capability]) =>
      `- ${cell(name)}: **${cell(capability.status)}** — ${cell(capability.reason)}`), "",
    "## Notes", "",
    "- 时间按会话 `created_at` 月份分组，不是事件发生时间。",
    "- sleep 秒数是一次调用中字面 sleep 段的合计；循环重复次数、并行和取消均未折算，不能当作实际墙钟。",
    "- 成功的 sleep 只表示命令退出，不表示等待目标达成；缺少 typed execution metadata 时状态为 unknown/missing。",
    "- 不能仅凭 sleep/echo 判定测试数据；真实构建、部署、服务启动也会等待。", "",
  ].join("\n");
}

export function writeWaitingReports(dto: WaitingDto, outDir: string): void {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(`${outDir}/waiting.json`, JSON.stringify(dto, null, 2));
  writeFileSync(`${outDir}/waiting.md`, renderWaitingMarkdown(dto));
  writeFileSync(`${outDir}/waiting.html`, renderWaitingHtml(dto));
}
