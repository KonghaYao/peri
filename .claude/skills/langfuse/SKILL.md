---
name: langfuse
description: Interact with Langfuse and access its documentation. Use when needing to (1) query or modify Langfuse data programmatically via the CLI — traces, prompts, datasets, scores, sessions, and any other API resource, (2) look up Langfuse documentation, concepts, integration guides, or SDK usage, or (3) understand how any Langfuse feature works. This skill covers CLI-based API access (via bunx) and multiple documentation retrieval methods.
allowed-tools:
  - WebFetch(domain:langfuse.com)
  - Bash(curl *langfuse.com/*)
  - Bash(bunx langfuse-cli api --help *)
  - Bash(bunx langfuse-cli api * --help *)
  - Bash(bunx langfuse-cli api * list *)
  - Bash(bunx langfuse-cli api * get *)
  - Bash(bun .claude/skills/langfuse/scripts/analyze.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/trace-search.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/trace-tree.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/trace-tokens.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/trace-messages.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/prompt-breakdown.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/traces-list.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/session-analyze.ts *)
  - Bash(bun .claude/skills/langfuse/scripts/daily-report.ts *)
---

# Langfuse

## 1. Langfuse API via CLI

Use `langfuse-cli` to interact with the full Langfuse REST API. Run via bunx (auto-loads `.env`):

```bash
bunx langfuse-cli api --help                              # 列出所有 resources
bunx langfuse-cli api <resource> --help                     # List actions for a resource
bunx langfuse-cli api <resource> <action> --help            # Show args for an action
bunx langfuse-cli api <resource> <action> [options]         # Execute
```

### Credentials

bunx automatically loads `.env`. Ensure it contains:

```bash
LANGFUSE_PUBLIC_KEY=pk-lf-...
LANGFUSE_SECRET_KEY=sk-lf-...
LANGFUSE_HOST=https://cloud.langfuse.com  # Required
```

If credentials are missing, ask the user to add them to `.env`. Do not ask to paste keys in chat.

### CLI Preflight and Query Integrity

Before attributing missing or malformed data to application behavior:

1. Confirm `LANGFUSE_HOST` or `LANGFUSE_BASE_URL` is set, credentials are present, and the selected host returns JSON rather than an HTML fallback. Never print credentials or authorization headers.
2. Discover the installed CLI schema with `bunx langfuse-cli api --help` and resource/action `--help`; resource names vary by CLI version, so do not assume `observations-v2s` or another historical alias exists.
3. For list endpoints, inspect pagination metadata and fetch every required page. The public API page limit is 100; a single page is not proof of completeness.
4. Record the requested field projection. If input/output fields were not requested or returned by the selected endpoint, report them as **not inspected**, not null or missing.
5. Stop business-level diagnosis on host/auth/schema failure. A 401, unsupported resource, HTML response, or truncated page set is a query precondition failure, not evidence about the trace producer.

### CLI Tips

- Use `--json` for machine-readable output
- Use `--curl` to preview HTTP request without executing
- Discover resources/actions with `--help`; do not hard-code version-specific v2 aliases
- Prefer the bundled scripts for traces and observations because they implement the current public endpoints and pagination

## 2. Data Retrieval Tools (脚本工具集)

All scripts accept common filtering options for time range and metadata:

| Option | Description | Example |
|--------|-------------|---------|
| `--from <ISO>` | Start timestamp | `--from 2026-07-01T00:00:00Z` |
| `--to <ISO>` | End timestamp | `--to 2026-07-31T23:59:59Z` |
| `--days <N>` | Last N days (from now) | `--days 7` |
| `--tag <tag>` | Filter by tag | `--tag production` |
| `--user <id>` | Filter by user ID | `--user user_123` |
| `--session <id>` | Filter by session ID | `--session sess_abc` |
| `--name <str>` | Filter by trace name | `--name chat` |
| `--limit <N>` | Max results | `--limit 50` |

### 2a. trace-search — 灵活搜索/过滤/导出

```bash
bun .claude/skills/langfuse/scripts/trace-search.ts [选项]

# 示例
bun .claude/skills/langfuse/scripts/trace-search.ts --days 7 --tag production                # 最近 7 天带 production tag 的 trace
bun .claude/skills/langfuse/scripts/trace-search.ts --session sess_abc --csv > session.csv   # 导出 session 为 CSV
bun .claude/skills/langfuse/scripts/trace-search.ts --model claude-sonnet --status error     # 查询特定模型的错误 trace
bun .claude/skills/langfuse/scripts/trace-search.ts --from 2026-07-01T00:00:00Z --summary   # 只看汇总统计
bun .claude/skills/langfuse/scripts/trace-search.ts --days 30 --json > report.json           # 导出 JSON
bun .claude/skills/langfuse/scripts/trace-search.ts --user user_123 --limit 100              # 按用户过滤
bun .claude/skills/langfuse/scripts/trace-search.ts --order latency.desc --limit 10          # 按延迟排序，找最慢的
```

Output modes: table (default), `--csv`, `--json`, `--summary` (aggregate only), `--full` (detailed fields).

### 2b. analyze — 成本/质量综合分析

```bash
bun .claude/skills/langfuse/scripts/analyze.ts [N]              # Overview + trace table + flags
bun .claude/skills/langfuse/scripts/analyze.ts --tools [N]      # Tool call analysis
bun .claude/skills/langfuse/scripts/analyze.ts --growth [N]     # Context growth trend
bun .claude/skills/langfuse/scripts/analyze.ts --report [N]     # Full report (all 7 sections)
bun .claude/skills/langfuse/scripts/analyze.ts --trace-id <id>  # Single trace detail

# 支持时间/元数据过滤
bun .claude/skills/langfuse/scripts/analyze.ts 20 --days 7 --user user_123 --report         # 某用户最近 7 天的完整报告
```

### 2c. session-analyze — Session 完整分析

```bash
bun .claude/skills/langfuse/scripts/session-analyze.ts --session <id> [选项]

# 选项
--limit <N>    最多拉取 trace 数（默认 100）
--detail       显示每个 trace 的逐轮 token 流
--csv          导出 CSV（每个 LLM 调用一行）

# 输出内容
# - Session 总体指标（traces, tokens, cost, time span）
# - Trace 时间线表格
# - 累积 token 增长趋势
# - 工具使用频率统计
# - 异常检测
```

### 2d. daily-report — 日报/周报

```bash
bun .claude/skills/langfuse/scripts/daily-report.ts [选项]

bun .claude/skills/langfuse/scripts/daily-report.ts                           # 今天的日报
bun .claude/skills/langfuse/scripts/daily-report.ts --days 7                  # 最近 7 天周报
bun .claude/skills/langfuse/scripts/daily-report.ts --days 30 --tag prod      # 按 tag 过滤的月报
bun .claude/skills/langfuse/scripts/daily-report.ts --model claude-sonnet     # 按模型过滤
bun .claude/skills/langfuse/scripts/daily-report.ts --detail                  # 显示所有 trace 详情

# 输出内容
# - Key Metrics（traces, sessions, errors, tokens, cost）
# - By Model 分布
# - Top Users（按输入 token）
# - Top Traces（按输入 token）
# - 异常 trace 列表
```

### 2e. 单 trace 深度分析

```bash
# Token 流 + 缓存异常
bun .claude/skills/langfuse/scripts/trace-tokens.ts <traceId>
bun .claude/skills/langfuse/scripts/trace-tokens.ts --index 1 --days 7        # 用 --index 从过滤结果中选 trace

# 消息组成 + diff
bun .claude/skills/langfuse/scripts/trace-messages.ts <traceId> [--detail]
bun .claude/skills/langfuse/scripts/trace-messages.ts --index 3 --user user_123

# System prompt 段落拆解
bun .claude/skills/langfuse/scripts/prompt-breakdown.ts <traceId>
bun .claude/skills/langfuse/scripts/prompt-breakdown.ts --index 1 --days 7

# Trace 汇总列表
bun .claude/skills/langfuse/scripts/traces-list.ts [N] [过滤选项]
```

### 2f. trace-tree — observation parent/orphan 审计

```bash
bun .claude/skills/langfuse/scripts/trace-tree.ts <traceId>
```

This command fetches all observation pages, prints a metadata-only tree, and exits non-zero when it finds duplicate IDs, missing parent observations, or cycles. A parent equal to the trace ID is a valid root attachment. Use it whenever the diagnosis concerns subagent ownership, generation/tool/batch nesting, or orphan observations; do not infer parent integrity from a flat list.

### Production Verification Gate

A unit/mock pass proves only local construction. After changing instrumentation or parent assignment:

1. Restart the actual producer process and record the new process/session provenance without exposing secrets.
2. Generate a new real trace after restart; do not reuse pre-fix data as acceptance evidence.
3. Run `trace-tree.ts` on that trace and inspect expected generation/tool/batch ownership.
4. Report code tests and production trace verification separately. If restart, credentials, or a live trace is unavailable, mark production verification blocked rather than complete.

## 3. Query Recipes（常见数据获取场景）

### 按时间查询

| 需求 | 命令 |
|------|------|
| 今天所有 trace | `bun .claude/skills/langfuse/scripts/daily-report.ts` 或 `bun .claude/skills/langfuse/scripts/trace-search.ts --days 1` |
| 本周 trace | `bun .claude/skills/langfuse/scripts/daily-report.ts --days 7` |
| 本月 trace | `bun .claude/skills/langfuse/scripts/daily-report.ts --days 30` |
| 特定时间段 | `bun .claude/skills/langfuse/scripts/trace-search.ts --from ISO --to ISO` |
| 上周 vs 本周对比 | 分别跑两次 `.claude/skills/langfuse/scripts/daily-report.ts --days 7`（注意时间不对齐），或用 `--from/--to` 精确控制 |

### 按用户/会话查询

| 需求 | 命令 |
|------|------|
| 某用户的所有 trace | `bun .claude/skills/langfuse/scripts/trace-search.ts --user <id> --days 30` |
| 某 session 完整分析 | `bun .claude/skills/langfuse/scripts/session-analyze.ts --session <id> --detail` |
| 某 session 导出 CSV | `bun .claude/skills/langfuse/scripts/session-analyze.ts --session <id> --csv` |
| 用户日报 | `bun .claude/skills/langfuse/scripts/daily-report.ts --user <id> --days 1` |

### 成本排查

| 需求 | 命令 |
|------|------|
| 找最贵的 trace | `bun .claude/skills/langfuse/scripts/trace-search.ts --order totalTokens --days 7 --limit 10` |
| 全量成本报告 | `bun .claude/skills/langfuse/scripts/analyze.ts 50 --days 7 --report` |
| 单模型成本 | `bun .claude/skills/langfuse/scripts/daily-report.ts --days 7 --model claude-sonnet` |
| 缓存效率低的 trace | `bun .claude/skills/langfuse/scripts/analyze.ts --days 7 --report`（看 Summary & Flags 的缓存异常） |

### 质量排查

| 需求 | 命令 |
|------|------|
| 找所有错误 trace | `bun .claude/skills/langfuse/scripts/trace-search.ts --status error --days 7` |
| 某错误 trace 深挖 | `bun .claude/skills/langfuse/scripts/trace-tokens.ts <traceId>` + `bun .claude/skills/langfuse/scripts/trace-messages.ts <traceId>` |
| agent loop 检测 | `bun .claude/skills/langfuse/scripts/analyze.ts --days 7 --tools`（看 LLM 调用次数） |
| context 膨胀分析 | `bun .claude/skills/langfuse/scripts/analyze.ts --growth --days 7` |

### 模型对比

| 需求 | 命令 |
|------|------|
| 模型用量分布 | `bun .claude/skills/langfuse/scripts/daily-report.ts --days 7`（看 By Model 表） |
| 某模型所有 trace | `bun .claude/skills/langfuse/scripts/trace-search.ts --model <model> --days 7 --csv` |

### 调试 Prompt

| 需求 | 命令 |
|------|------|
| 看 system prompt 结构 | `bun .claude/skills/langfuse/scripts/prompt-breakdown.ts --index 1 --days 1` |
| system prompt 是否稳定 | `bun .claude/skills/langfuse/scripts/trace-messages.ts <traceId>`（看 System Prompt Stability 段落） |
| 上下文增长来源 | `bun .claude/skills/langfuse/scripts/trace-messages.ts <traceId> --detail`（看消息 diff） |

## 4. Data Retrieval Patterns（按目的选择工具）

### 日常监控 → `daily-report.ts`
快速了解系统状态：今天/本周有多少 trace、花了多少钱、有没有异常。每天跑一次即可。

### 深入问题诊断 → `analyze.ts --report`
当发现异常（成本飙升、缓存降低、用户反馈质量差）时，对最近 N 条 trace 做全维度扫描。

### 精准搜索 → `trace-search.ts`
当你已经知道要找什么（某用户、某 session、某时间段、某模型），直接筛选。支持导出 CSV/JSON 做进一步分析。

### 单条追踪 → `trace-tokens.ts` + `trace-messages.ts` + `prompt-breakdown.ts`
定位到具体 trace 后，这三件套分别看 token 流、消息变化、prompt 结构，逐轮定位问题。

### Session 回溯 → `session-analyze.ts`
需要完整还原用户的一次会话时使用，看 trace 时间线、token 累积、工具使用演变。

### Prompt 工程 → `prompt-breakdown.ts` + CLI get prompt
先看现有 request 中 system prompt 的段落分布（哪些段落最大），然后用 CLI 管理 Langfuse prompt：

```bash
bunx langfuse-cli api prompts list
bunx langfuse-cli api prompts get --name <name>
bunx langfuse-cli api prompts create --name <name> --type chat --prompt '[...]'
```

## 5. Cost Analysis（详细版）

### Report Sections

| # | Section | What it shows |
|---|---------|---------------|
| 1 | Overview | Aggregate stats, cache efficiency, output/input ratio |
| 2 | Per-Trace Table | Input/output/cache/latency per trace |
| 3 | Tool Analysis | Frequency, avg latency, redundancy detection, tool→context growth |
| 4 | Context Growth | Per-trace token trend (visual bar chart), session accumulation, cross-trace growth rate |
| 5 | System Prompt Occupancy | Section breakdown with estimated tokens, system vs conversation ratio |
| 6 | Most Expensive Trace | Per-LLM-call detail with delta |
| 7 | Summary & Flags | Auto-detected issues (low cache, redundant tools, slow calls, etc.) |

### Red Flags

| Pattern | Threshold | Root Cause |
|---------|-----------|------------|
| Cache hit rate < 90% | Single trace | System prompt instability, cold start, or structure changing across turns |
| Effective new tokens > 20K | Single trace | Tool results or context growing unbounded |
| Output/Input ratio > 5% | Single trace | Model over-explaining |
| Output/Input ratio < 0.1% | Single trace | Massive input for tiny output — unnecessary context |
| LLM calls > 10 for simple task | Single trace | Agent looping or retrying |
| Single LLM call > 60s | Per-call | Model generating too much for the task |

### Optimization Checklist

After analysis, evaluate:

1. **System Prompt Weight** — >40% of context → trim; largest section → shorten or lazy-load; stale CLAUDE.md TRAPs → archive
2. **Context Accumulation** — tool results retained across turns?; micro-compact threshold right?; redundant reads?
3. **Agent Loop Efficiency** — redundant tool calls?; sequential reads → batch?; broad exploration → targeted search?
4. **Task Decomposition** — complex task → focused sub-tasks?; sub-agents to reduce context pressure?

### Reflection Output Format

```
## Cost Reflection

### Metrics
- Traces analyzed: N
- Total input: X tokens (Y% cache hit)
- Total output: Z tokens
- Avg LLM calls per trace: M

### Findings
1. [Pattern with specific trace example]
2. [Another pattern]

### Recommendations
1. [Actionable optimization] — estimated savings: ~X tokens/trace
2. [Another recommendation]
```

## 6. Langfuse Documentation

### 6a. Documentation Index (llms.txt)

```bash
curl -s https://langfuse.com/llms.txt
```

Returns structured list of every doc page. Use to discover the right page, then fetch it.

### 6b. Fetch Pages as Markdown

Append `.md` to any doc path:

```bash
curl -s "https://langfuse.com/docs/observability/overview.md"
```

### 6c. Search Documentation

```bash
curl -s "https://langfuse.com/api/search-docs?query=How+do+I+trace+LangGraph+agents"
```

Returns matching documents with URLs, titles, and excerpts. Also indexes GitHub Issues/Discussions.

### Workflow

1. Start with **llms.txt** to orient
2. **Fetch specific pages** when identified
3. Fall back to **search** when topic is unclear

## 7. 上下文 Diff 诊断（对比两次 LLM 调用的完整输入）

当不同 trace/session 的 input tokens 存在无法解释的差异时，下载完整 input 做 diff 是最直接的定位手段。

### 步骤

**1. 找到差异 trace 的 generation observation ID**

```bash
# 列出 session 的所有 trace
bunx langfuse-cli api traces list --session-id <session_id> --json | jq '.body.data[].id'

# 列出 trace 下所有 GENERATION observation
curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  "$LANGFUSE_HOST/api/public/observations?traceId=<trace_id>&limit=100" \
  | jq '[.data[] | select(.type == "GENERATION") | {id, inputTokens: .usageDetails.input}]'
```

**2. 下载完整 input 并保存**

```bash
curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  "$LANGFUSE_HOST/api/public/observations/<obs_id>" \
  | jq '.input' > /tmp/input_a.json

curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  "$LANGFUSE_HOST/api/public/observations/<obs_id>" \
  | jq '.input' > /tmp/input_b.json
```

**3. Diff**

```bash
diff /tmp/input_a.json /tmp/input_b.json
```

### 典型场景

| 场景 | 表现 | Diff 会发现 |
|------|------|------------|
| System prompt 不稳定 | 同模型同会话类型但 input tokens 差异大 | `messages[0].content`（system prompt）中某段内容不同 |
| Tools 数组变化 | input tokens 差异 ~数 K | `tools` 数组长度或内容不同 |
| Deferred Tools / MCP 描述 | 跨会话缓存命中率为 0% | system prompt 中 `Deferred Tools` 段多了/少了 MCP 工具描述文本 |
| 消息历史差异 | 上下文增长异常 | `messages` 数组长度不同，某条消息缺失或重复 |

### 注意

- `.input` 是完整请求体（包含 `messages`、`tools`、`model` 等字段），diff 能精确定位任何差异
- 如果只需要比 system prompt：`jq '.input.messages[0].content' -r`
- 如果只需要比 tools：`jq '.input.tools'`
- Generation observation 的 `usageDetails` 包含 `cache_read_input_tokens` 和 `cache_creation_input_tokens`，是缓存诊断的关键数据

## Use Case References

- instrumenting an application: references/instrumentation.md
- migrating prompts: references/prompt-migration.md
- user feedback as scores: references/user-feedback.md
- CLI tips: references/cli.md
- SDK upgrade: references/sdk-upgrade.md
- judge calibration: references/judge-calibration.md
- error analysis: references/error-analysis.md
- skill feedback: references/skill-feedback.md
