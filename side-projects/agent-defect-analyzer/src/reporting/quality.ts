import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { DataLoader, SchemaCompatibilityError, normalizeMessage } from "../data/loader.js";

export type QualityStatus = "pass" | "warn" | "fail" | "unavailable";

export interface QualityCheck {
  id: string;
  status: QualityStatus;
  numerator: number | null;
  denominator: number | null;
  rate: number | null;
  unit: string;
  scope: string;
  definition: string;
  evidence_ids: string[];
  reason?: string;
}

export interface InspectDto {
  schema_version: "inspect.v1";
  source: {
    snapshot_fingerprint: string;
    generated_at: string;
    schema_user_version: number;
    tables: string[];
    optional_missing: Record<string, string[]>;
    time_range: { created_min: string | null; updated_max: string | null };
  };
  scope: {
    threads: { total: number; roots: number; children: number; hidden: number };
    messages: {
      total: number;
      own: number;
      inherited: number;
      snapshot_inherited: number;
      snapshot_inherited_errors: number;
      orphan_rows: number;
      by_role: Record<string, number>;
      count_mismatch_threads: number;
      count_mismatch_messages: number;
    };
  };
  format: {
    assistant_messages: number;
    content_tool_use: number;
    top_level_tool_calls: number;
    dual_write_messages: number;
    duplicate_tool_use_candidates: number;
    ambiguous_tool_uses: number;
    tool_use_count: number;
    tool_result_count: number;
    tool_result_error_count: number;
    tool_result_unknown_error_count: number;
    raw_tool_result_rows: number;
    rejected_tool_result_rows: number;
    rejected_tool_error_rows: number;
    tool_names: Record<string, number>;
  };
  canonical: {
    excluded_messages: number;
    included_messages: number;
    truncated_messages: number;
    projection_messages: number;
  };
  parsing: {
    json_ok: number;
    json_fail: number;
    empty_content: number;
    role_mismatch: number;
    unknown_role: number;
    normalization_issues: Record<string, number>;
  };
  pairing: {
    paired_results: number;
    orphan_results: number;
    unmatched_uses: number;
  };
  evidence: { ids: string[]; truncated: boolean };
  checks: QualityCheck[];
  capabilities: Record<string, { status: QualityStatus; reason?: string }>;
}

function asString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function asBool(value: unknown): boolean {
  return value === true || value === 1 || value === "1";
}

function rate(numerator: number, denominator: number): number | null {
  return denominator === 0 ? null : numerator / denominator;
}

function check(
  id: string,
  status: QualityStatus,
  numerator: number | null,
  denominator: number | null,
  unit: string,
  scope: string,
  definition: string,
  evidence_ids: string[],
  reason?: string,
): QualityCheck {
  return {
    id,
    status,
    numerator,
    denominator,
    rate: numerator === null || denominator === null ? null : rate(numerator, denominator),
    unit,
    scope,
    definition,
    evidence_ids,
    ...(reason ? { reason } : {}),
  };
}

function evidenceId(threadId: string, messageId: string): string {
  return `thread:${threadId}:message:${messageId}`;
}

function addEvidence(ids: string[], id: string, max = 50): void {
  if (ids.length < max && !ids.includes(id)) ids.push(id);
}

function markdown(dto: InspectDto): string {
  const pct = (value: number | null) => value === null ? "unavailable" : `${(value * 100).toFixed(2)}%`;
  const cell = (value: unknown): string => String(value ?? "").replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll("|", "\\|").replaceAll("\n", " ").replaceAll("\r", " ");
  const checkRows = dto.checks.map((c) => `| ${cell(c.id)} | ${cell(c.status)} | ${cell(c.numerator ?? "-")} | ${cell(c.denominator ?? "-")} | ${cell(pct(c.rate))} | ${cell(c.reason ?? "")} |`);
  const tools = Object.entries(dto.format.tool_names).sort((a, b) => b[1] - a[1]).map(([name, count]) => `| ${cell(name)} | ${count} |`).join("\n") || "| - | 0 |";
  return [
    "# Inspect 数据质量",
    "",
    `- schema: ${dto.schema_version}`,
    `- snapshot fingerprint: \`${dto.source.snapshot_fingerprint}\``,
    `- coverage boundary: created_min=${dto.source.time_range.created_min ?? "-"}, updated_max=${dto.source.time_range.updated_max ?? "-"}`,
    `- schema metadata: user_version=${dto.source.schema_user_version}, tables=${dto.source.tables.join(",")}`,
    "- 口径：全库质量检查；roots = `parent_thread_id IS NULL`，children = 有 parent 的线程；消息行按其持久化 thread 归属，snapshot inherited 单独计数；excluded 保留并独立计数。",
    "- 默认报告不包含原始消息、路径或参数；evidence 仅列本机可查证 ID。",
    "",
    "## Scope",
    "",
    `- threads: total=${dto.scope.threads.total}, roots=${dto.scope.threads.roots}, children=${dto.scope.threads.children}, hidden=${dto.scope.threads.hidden}`,
    `- messages: persisted_total=${dto.scope.messages.total}, own_rows=${dto.scope.messages.own}, inherited_rows=${dto.scope.messages.inherited}, orphan_rows=${dto.scope.messages.orphan_rows}, snapshot_inherited=${dto.scope.messages.snapshot_inherited}, snapshot_errors=${dto.scope.messages.snapshot_inherited_errors}`,
    `- roles: ${JSON.stringify(dto.scope.messages.by_role)}`,
    "",
    "## Canonical records",
    "",
    `- excluded=${dto.canonical.excluded_messages}, included=${dto.canonical.included_messages}, truncated=${dto.canonical.truncated_messages}, projection=${dto.canonical.projection_messages}`,
    "",
    "## Quality checks",
    "",
    "| id | status | numerator | denominator | rate | reason |",
    "| --- | --- | ---: | ---: | ---: | --- |",
    ...checkRows,
    "",
    "## Tool facts",
    "",
    `- tool uses=${dto.format.tool_use_count}, duplicate candidates=${dto.format.duplicate_tool_use_candidates}, tool results=${dto.format.tool_result_count}, errors=${dto.format.tool_result_error_count}`,
    `- raw tool rows=${dto.format.raw_tool_result_rows}, rejected rows=${dto.format.rejected_tool_result_rows}, rejected error rows=${dto.format.rejected_tool_error_rows}, unknown error state=${dto.format.tool_result_unknown_error_count}`,
    `- paired results=${dto.pairing.paired_results}, orphan results=${dto.pairing.orphan_results}, unmatched uses=${dto.pairing.unmatched_uses}`,
    "",
    "| tool | uses |",
    "| --- | ---: |",
    tools,
    "",
    "## Capabilities",
    "",
    ...Object.entries(dto.capabilities).map(([name, value]) => `- ${name}: **${value.status}**${value.reason ? ` — ${value.reason}` : ""}`),
    "",
    `Evidence IDs retained: ${dto.evidence.ids.length}${dto.evidence.truncated ? " (capped)" : ""}`,
    "",
  ].join("\n");
}

export function writeQualityReports(dto: InspectDto, outDir: string): void {
  mkdirSync(outDir, { recursive: true });
  writeFileSync(`${outDir}/quality.json`, JSON.stringify(dto, null, 2));
  writeFileSync(`${outDir}/quality.md`, markdown(dto));
}

function emptyInspectDto(now: string, reason: string, table?: string): InspectDto {
  return {
    schema_version: "inspect.v1",
    source: { snapshot_fingerprint: "unavailable", generated_at: now, schema_user_version: 0, tables: [], optional_missing: {}, time_range: { created_min: null, updated_max: null } },
    scope: { threads: { total: 0, roots: 0, children: 0, hidden: 0 }, messages: { total: 0, own: 0, inherited: 0, orphan_rows: 0, snapshot_inherited: 0, snapshot_inherited_errors: 0, by_role: {}, count_mismatch_threads: 0, count_mismatch_messages: 0 } },
    format: { assistant_messages: 0, content_tool_use: 0, top_level_tool_calls: 0, dual_write_messages: 0, duplicate_tool_use_candidates: 0, ambiguous_tool_uses: 0, tool_use_count: 0, tool_result_count: 0, tool_result_error_count: 0, tool_result_unknown_error_count: 0, raw_tool_result_rows: 0, rejected_tool_result_rows: 0, rejected_tool_error_rows: 0, tool_names: {} },
    canonical: { excluded_messages: 0, included_messages: 0, truncated_messages: 0, projection_messages: 0 },
    parsing: { json_ok: 0, json_fail: 0, empty_content: 0, role_mismatch: 0, unknown_role: 0, normalization_issues: {} },
    pairing: { paired_results: 0, orphan_results: 0, unmatched_uses: 0 },
    evidence: { ids: [], truncated: false },
    checks: [check(table ? "schema.required_columns" : "schema.open", "fail", 0, null, "database", "full", "read-only data loader can open the declared schema", [], reason)],
    capabilities: { normalized_messages: { status: "unavailable", reason } },
  };
}

export function inspectDatabase(dbPath: string): InspectDto {
  let loader: DataLoader;
  try {
    loader = new DataLoader(dbPath);
  } catch (error) {
    const reason = error instanceof SchemaCompatibilityError ? error.message : `unable to open database: ${error instanceof Error ? error.message : String(error)}`;
    const now = new Date().toISOString();
    return emptyInspectDto(now, reason, error instanceof SchemaCompatibilityError ? error.table : undefined);
  }

  try {
    return loader.withSnapshot<InspectDto>((source) => {
      const capabilities = source.capabilities;
      const threadRows = source.loadThreadSummaries();
      const byThread = new Map(threadRows.map((thread) => [thread.id, thread]));
      const digest = createHash("sha256");
      digest.update(JSON.stringify({ schema_version: "inspect.v1", capabilities }));
      const evidence: string[] = [];
      const parseEvidence: string[] = [];
      const normalizationEvidence: string[] = [];
      const reconciliationEvidence: string[] = [];
      const snapshotEvidence: string[] = [];
      const pairingEvidence: string[] = [];
      const formatEvidence: string[] = [];
      const ambiguousEvidence: string[] = [];
      const roleEvidence: string[] = [];
      let evidenceCapped = false;
      const recordEvidence = (id: string, ...scoped: string[][]): void => {
        if (evidence.length < 50) addEvidence(evidence, id); else evidenceCapped = true;
        for (const ids of scoped) addEvidence(ids, id);
      };
      let hidden = 0;
      let rootThreads = 0;
      let childThreads = 0;
      let snapshotInherited = 0;
      let snapshotInheritedErrors = 0;
      let createdMin: string | null = null;
      let updatedMax: string | null = null;
      for (const row of threadRows) {
        const parent = asString(row.parent_thread_id);
        if (parent) childThreads++; else rootThreads++;
        if (asBool(row.hidden)) hidden++;
        try {
          const inherited = source.loadInheritedMessages(row.id);
          snapshotInherited += inherited.length;
          for (const message of inherited) digest.update(JSON.stringify({ inherited: true, thread_id: message.threadId, message_id: message.messageId, role: message.role, text: message.text, excluded: message.excludedFromContext, truncated: message.truncated }));
        }
        catch { snapshotInheritedErrors++; recordEvidence(`thread:${row.id}`, snapshotEvidence); }
        if (row.created_at && (!createdMin || row.created_at < createdMin)) createdMin = row.created_at;
        if (row.updated_at && (!updatedMax || row.updated_at > updatedMax)) updatedMax = row.updated_at;
        digest.update(JSON.stringify({ id: row.id, parent_thread_id: row.parent_thread_id, hidden: row.hidden, message_count: row.message_count, created_at: row.created_at, updated_at: row.updated_at, snapshot_at_message_id: row.snapshot_at_message_id }));
      }

      const roleCounts: Record<string, number> = {};
      const actualByThread = new Map<string, number>();
      let messageTotal = 0;
      let jsonOk = 0;
      let jsonFail = 0;
      let emptyContent = 0;
      let roleMismatch = 0;
      let unknownRole = 0;
      const normalizationIssues: Record<string, number> = {};
      let excluded = 0;
      let truncated = 0;
      let projection = 0;
      let assistantMessages = 0;
      let contentToolUse = 0;
      let topToolCalls = 0;
      let dualWrite = 0;
      let duplicateToolUseCandidates = 0;
      let ambiguous = 0;
      let toolUseCount = 0;
      let toolResultCount = 0;
      let toolResultErrors = 0;
      let toolResultUnknownErrors = 0;
      let rawToolResultRows = 0;
      let rejectedToolErrorRows = 0;
      const toolNames: Record<string, number> = {};
      const knownRoles = new Set(["user", "assistant", "system", "system_reminder", "tool"]);

      // Normalize and pair one thread at a time. DataLoader orders rows by
      // thread_id, so orphan rows are also visited without loading all rows.
      let pairedResults = 0;
      let orphanResults = 0;
      let unmatchedUses = 0;
      let activeThreadId: string | null = null;
      let useKeys = new Set<string>();
      let resultRows: { messageId: string; callId: string }[] = [];
      const finishThread = (): void => {
        if (activeThreadId === null) return;
        const pairedUseKeys = new Set<string>();
        for (const result of resultRows) {
          if (result.callId && useKeys.has(result.callId)) { pairedResults++; pairedUseKeys.add(result.callId); }
          else { orphanResults++; recordEvidence(evidenceId(activeThreadId, result.messageId), pairingEvidence); }
        }
        unmatchedUses += [...useKeys].filter((key) => !pairedUseKeys.has(key)).length;
      };
      for (const row of source.iterateMessages()) {
        if (activeThreadId !== row.thread_id) {
          finishThread();
          activeThreadId = row.thread_id;
          useKeys = new Set<string>();
          resultRows = [];
        }
        const message = normalizeMessage(row);
        const threadId = row.thread_id;
        const messageId = row.message_id || `sequence:${row.sequence}`;
        const id = evidenceId(threadId, messageId);
        messageTotal++;
        actualByThread.set(threadId, (actualByThread.get(threadId) ?? 0) + 1);
        roleCounts[row.role] = (roleCounts[row.role] ?? 0) + 1;
        digest.update(JSON.stringify({ thread_id: threadId, message_id: messageId, role: row.role, content: row.content, excluded: row.excluded, truncated: row.truncated, projection: row.projection }));
        if (typeof row.content !== "string" || row.content.length === 0) emptyContent++;
        for (const issue of message.parseIssues) {
          const category = issue.includes(":") ? issue.slice(0, issue.indexOf(":")) : issue;
          normalizationIssues[category] = (normalizationIssues[category] ?? 0) + 1;
        }
        const invalid = message.parseIssues.some((issue) => issue === "invalidJson" || issue === "payload_not_object");
        if (invalid) { jsonFail++; recordEvidence(id, parseEvidence, normalizationEvidence); } else jsonOk++;
        if (message.parseIssues.length > 0) recordEvidence(id, normalizationEvidence);
        if (message.parseIssues.includes("role_mismatch")) { roleMismatch++; recordEvidence(id, roleEvidence); }
        if (!knownRoles.has(row.role)) { unknownRole++; recordEvidence(id, roleEvidence); }
        if (asBool(row.excluded)) excluded++;
        if (asBool(row.truncated)) truncated++;
        if (asString(row.projection)) projection++;
        if (row.role === "assistant") {
          assistantMessages++;
          const contentCalls = message.calls.filter((call) => call.sources.includes("content"));
          const topCalls = message.calls.filter((call) => call.sources.includes("tool_calls"));
          contentToolUse += contentCalls.length;
          topToolCalls += topCalls.length;
          const duplicateCalls = message.calls.filter((call) => call.sources.includes("content") && call.sources.includes("tool_calls")).length;
          if (duplicateCalls > 0) { dualWrite++; duplicateToolUseCandidates += duplicateCalls; addEvidence(formatEvidence, id); }
          for (const call of message.calls) {
            toolUseCount++;
            toolNames[call.name] = (toolNames[call.name] ?? 0) + 1;
            if (call.id) useKeys.add(call.id);
          }
          ambiguous += message.parseIssues.filter((issue) => issue.includes("missing_id_or_name")).length;
          if (message.parseIssues.some((issue) => issue.includes("missing_id_or_name"))) { recordEvidence(id); addEvidence(ambiguousEvidence, id); }
        }
        for (const result of message.results) {
          toolResultCount++;
          if (result.isError === true) toolResultErrors++;
          if (result.isError === null) toolResultUnknownErrors++;
          resultRows.push({ messageId, callId: result.id });
        }
        if (row.role === "tool") {
          rawToolResultRows++;
          if (message.parseIssues.includes("tool_message_missing_id")) rejectedToolErrorRows++;
        }
      }
      finishThread();
      let mismatchThreads = 0;
      let mismatchMessages = 0;
      const orphanRows = [...actualByThread.keys()].filter((id) => !byThread.has(id)).reduce((sum, id) => sum + (actualByThread.get(id) ?? 0), 0);
      for (const [id, thread] of byThread) {
        const actual = actualByThread.get(id) ?? 0;
        if (Number.isFinite(thread.message_count) && thread.message_count !== actual) {
          mismatchThreads++;
          mismatchMessages += Math.abs(thread.message_count - actual);
          recordEvidence(`thread:${id}`, reconciliationEvidence);
        }
      }
      evidenceCapped = evidence.length >= 50;
      const now = new Date().toISOString();
      const requiredThreadColumns = ["id", "title", "cwd", "created_at", "updated_at", "message_count"];
      const requiredMessageColumns = ["message_id", "thread_id", "role", "content"];
      const threadColumns = capabilities.columns.threads ?? [];
      const messageColumns = capabilities.columns.messages ?? [];
      const columnNumerator = requiredThreadColumns.filter((column) => threadColumns.includes(column)).length + requiredMessageColumns.filter((column) => messageColumns.includes(column)).length;
      const checks: QualityCheck[] = [
        check("schema.required_tables", capabilities.tables.includes("threads") && capabilities.tables.includes("messages") ? "pass" : "fail", 2, 2, "tables", "full", "required threads and messages tables exist", []),
        check("schema.required_columns", columnNumerator === requiredThreadColumns.length + requiredMessageColumns.length ? "pass" : "fail", columnNumerator, requiredThreadColumns.length + requiredMessageColumns.length, "columns", "full", "loader contract columns are present", [], columnNumerator === requiredThreadColumns.length + requiredMessageColumns.length ? undefined : `threads: ${threadColumns.join(",")}; messages: ${messageColumns.join(",")}`),
        check("messages.parse_fail", jsonFail === 0 ? "pass" : "warn", jsonFail, messageTotal, "messages", "full", "message content is valid JSON object", parseEvidence),
        check("messages.normalization_issues", Object.keys(normalizationIssues).length === 0 ? "pass" : "warn", Object.values(normalizationIssues).reduce((sum, count) => sum + count, 0), messageTotal, "issues", "full", "normalized payload has no observable schema/format issues", normalizationEvidence, Object.keys(normalizationIssues).length ? JSON.stringify(normalizationIssues) : undefined),
        check("messages.count_reconciliation", mismatchThreads === 0 ? "pass" : "warn", mismatchThreads, threadRows.length, "threads", "full", "threads.message_count equals persisted message rows", reconciliationEvidence, mismatchThreads ? `${mismatchThreads} thread(s) differ by ${mismatchMessages} message(s)` : undefined),
        check("messages.orphan_rows", orphanRows === 0 ? "pass" : "warn", orphanRows, messageTotal, "messages", "full", "every persisted message references a known thread", reconciliationEvidence, orphanRows ? `${orphanRows} message row(s) reference missing threads` : undefined),
        check("messages.snapshot_inherited", snapshotInheritedErrors === 0 ? "pass" : "warn", snapshotInheritedErrors, threadRows.length, "threads", "full", "inherited_context payloads normalize without errors", snapshotEvidence, snapshotInheritedErrors ? `${snapshotInheritedErrors} thread(s) failed inherited snapshot normalization` : undefined),
        check("tool_results.pairing", orphanResults === 0 && unmatchedUses === 0 ? "pass" : "warn", pairedResults, toolResultCount, "tool_results", "full", "tool results pair by thread and tool call ID", pairingEvidence, `orphans=${orphanResults}; unmatched_uses=${unmatchedUses}`),
        check("format.dual_write", dualWrite === 0 ? "pass" : "warn", dualWrite, assistantMessages, "assistant_messages", "full", "normalized duplicate call IDs identify content/top-level dual writes", formatEvidence),
        check("format.ambiguous_tool_use", ambiguous === 0 ? "pass" : "warn", ambiguous, toolUseCount + ambiguous, "tool_uses", "full", "tool uses have stable IDs and names", ambiguousEvidence),
        check("messages.role_contract", roleMismatch === 0 && unknownRole === 0 ? "pass" : "warn", roleMismatch + unknownRole, messageTotal, "messages", "full", "stored role agrees with normalized payload and known roles", roleEvidence, `role_mismatch=${roleMismatch}; unknown_role=${unknownRole}`),
      ];
      const qualityCapabilities: Record<string, { status: QualityStatus; reason?: string }> = {
        normalized_messages: { status: jsonFail === 0 && Object.keys(normalizationIssues).length === 0 ? "pass" : "warn", ...((jsonFail || Object.keys(normalizationIssues).length) ? { reason: `${jsonFail} JSON parse failure(s); normalization issues=${JSON.stringify(normalizationIssues)}` } : {}) },
        tool_result_error_rate: toolResultCount > 0 && orphanResults === 0 ? { status: "pass" } : { status: "unavailable", reason: "error rate requires paired tool results" },
        token_usage: { status: "unavailable", reason: "no stable token usage source is included in inspect" },
        user_satisfaction: { status: "unavailable", reason: "requires explicit human evidence" },
      };
      return {
        schema_version: "inspect.v1",
        source: { snapshot_fingerprint: `sha256:${digest.digest("hex")}`, generated_at: now, schema_user_version: capabilities.userVersion, tables: capabilities.tables, optional_missing: capabilities.optionalMissing, time_range: { created_min: createdMin, updated_max: updatedMax } },
        scope: { threads: { total: threadRows.length, roots: rootThreads, children: childThreads, hidden }, messages: { total: messageTotal, own: messageTotal - orphanRows, inherited: 0, orphan_rows: orphanRows, snapshot_inherited: snapshotInherited, snapshot_inherited_errors: snapshotInheritedErrors, by_role: roleCounts, count_mismatch_threads: mismatchThreads, count_mismatch_messages: mismatchMessages } },
        format: { assistant_messages: assistantMessages, content_tool_use: contentToolUse, top_level_tool_calls: topToolCalls, dual_write_messages: dualWrite, duplicate_tool_use_candidates: duplicateToolUseCandidates, ambiguous_tool_uses: ambiguous, tool_use_count: toolUseCount, tool_result_count: toolResultCount, tool_result_error_count: toolResultErrors, tool_result_unknown_error_count: toolResultUnknownErrors, raw_tool_result_rows: rawToolResultRows, rejected_tool_result_rows: Math.max(0, rawToolResultRows - toolResultCount), rejected_tool_error_rows: rejectedToolErrorRows, tool_names: toolNames },
        canonical: { excluded_messages: excluded, included_messages: messageTotal - excluded, truncated_messages: truncated, projection_messages: projection },
        parsing: { json_ok: jsonOk, json_fail: jsonFail, empty_content: emptyContent, role_mismatch: roleMismatch, unknown_role: unknownRole, normalization_issues: normalizationIssues },
        pairing: { paired_results: pairedResults, orphan_results: orphanResults, unmatched_uses: unmatchedUses },
        evidence: { ids: evidence, truncated: evidenceCapped },
        checks,
        capabilities: qualityCapabilities,
      };
    }).value;
  } finally {
    loader.close();
  }
}

export { markdown as renderQualityMarkdown };
