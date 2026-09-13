# Agent behavior report

- schema: report.v1
- source fingerprint: 258472bf423399fbd0d40e2c24569122a91e075adc1380de8c0b8a91423bace4
- normalizer: peri-normalizer-v2; metrics: agent-metrics-v1
- filters: scope=roots, include_hidden=false, since=2026-09-01T00:00:00Z, until=2026-09-13T00:00:00Z
- 默认输出只包含统计、规则和 evidence IDs，不包含原始消息、参数或路径。

## Totals

- threads=204, messages=33961, calls=19891
- explicit_errors=323, unknown_error_results=0, parse_issues=0

## Top tool errors

| tool | errors | affected threads | known result denominator | error rate |
| --- | ---: | ---: | ---: | ---: |
| Edit | 84 | 41 | 3667 | 2.29% |
| Read | 69 | 44 | 5298 | 1.30% |
| Agent | 41 | 17 | 347 | 11.82% |
| ExecuteExtraTool→Workflow | 24 | 12 | 61 | 39.34% |
| Bash | 21 | 18 | 4235 | 0.50% |
| StrReplace | 19 | 6 | 19 | 100.00% |
| WebFetch | 14 | 6 | 66 | 21.21% |
| Grep | 13 | 9 | 3656 | 0.36% |
| AskUserQuestion | 9 | 8 | 91 | 9.89% |
| Glob | 7 | 5 | 589 | 1.19% |
| Write | 6 | 3 | 260 | 2.31% |
| folder_operations | 2 | 2 | 288 | 0.69% |
| WebSearch | 2 | 2 | 54 | 3.70% |
| Workflow | 2 | 1 | 8 | 25.00% |
| Delete | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_logs | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp__openship__get_system_servers | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→RunPtcCode | 1 | 1 | 20 | 5.00% |
| Gash | 1 | 1 | 1 | 100.00% |
| mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | 1 | 1 | 1 | 100.00% |
| Shell | 1 | 1 | 36 | 2.78% |
| SkillTool | 1 | 1 | 118 | 0.85% |

## Pairing

- paired=19891/19891 (100.00%)
- known state=19891/19891 (100.00%)
- missing=0, orphan=0, duplicate calls=0, duplicate results=0

## Result bytes

- count=19891, p50=631, p95=10035, giant=1, error_bytes=93680

## Rules

| rule | kind | threshold | numerator | denominator | rate | next verification |
| --- | --- | --- | ---: | ---: | ---: | --- |
| pairing-quality | quality | paired results / normalized tool calls; known result state / paired results | 19891 | 19891 | 100.00% | Inspect missing and orphan result evidence before using error rates for behavior claims. |
| known-result-state | quality | paired results with explicit success or error state / paired results | 19891 | 19891 | 100.00% | Review unknown result states and source serialization before treating error rates as complete. |
| explicit-failure | candidate | the same labeled tool produces at least 2 consecutive explicit errors within a thread | 28 | 19891 | 0.14% | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| repeated-call | candidate | same canonical call and arguments occur at least 3 times consecutively within a thread | 2 | 19891 | 0.01% | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| large-tool-output | observation | UTF-8 result content is at least 100000 bytes | 1 | 19891 | 0.01% | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Candidates

| rule | count | denominator | evidence IDs | next verification |
| --- | ---: | ---: | ---: | --- |
| explicit-failure | 28 | 19891 | 8 | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| repeated-call | 2 | 19891 | 2 | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| large-tool-output | 1 | 19891 | 1 | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Capabilities

- tool_errors: **available** — paired normalized tool results with explicit error state
- repeated_calls: **available** — same labeled tool and stable canonical arguments within a thread
- result_bytes: **available** — UTF-8 byte counts from normalized tool results
- token_usage: **unavailable** — no stable token source in the analysis contract
- latency: **unavailable** — persisted event timestamps are not a stable execution duration source
- completion: **unavailable** — completion requires an explicit outcome label or human evidence
