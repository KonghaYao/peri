# Agent behavior report

- schema: report.v1
- source fingerprint: 8d16f95b9543e58bf900962c95e614a27a7fb72f5f88f503e88b502a22d5770f
- normalizer: peri-normalizer-v2; metrics: agent-metrics-v1
- filters: scope=roots, include_hidden=false, since=-, until=-
- 默认输出只包含统计、规则和 evidence IDs，不包含原始消息、参数或路径。

## Totals

- threads=4586, messages=351978, calls=183375
- explicit_errors=3508, unknown_error_results=0, parse_issues=21

## Top tool errors

| tool | errors | affected threads | known result denominator | error rate |
| --- | ---: | ---: | ---: | ---: |
| Edit | 995 | 449 | 28156 | 3.53% |
| Read | 835 | 463 | 53262 | 1.57% |
| Bash | 628 | 355 | 49275 | 1.27% |
| Agent | 353 | 175 | 5194 | 6.80% |
| Grep | 149 | 99 | 25717 | 0.58% |
| AskUserQuestion | 112 | 75 | 1577 | 7.10% |
| WebFetch | 88 | 43 | 376 | 23.40% |
| ExecuteExtraTool→Workflow | 48 | 29 | 393 | 12.21% |
| AgentResult | 44 | 15 | 1043 | 4.22% |
| run_code | 29 | 2 | 52 | 55.77% |
| folder_operations | 24 | 20 | 1481 | 1.62% |
| ExecuteExtraTool | 22 | 6 | 22 | 100.00% |
| Glob | 20 | 12 | 3385 | 0.59% |
| StrReplace | 19 | 6 | 19 | 100.00% |
| LSP | 18 | 5 | 26 | 69.23% |
| WebSearch | 17 | 12 | 334 | 5.09% |
| Write | 14 | 8 | 3641 | 0.38% |
| ExecuteExtraTool→RunPtcCode | 13 | 2 | 44 | 29.55% |
| LineEdit | 6 | 6 | 326 | 1.84% |
| ExecuteExtraTool→Goal | 6 | 2 | 16 | 37.50% |
| TodoWrite | 5 | 5 | 7159 | 0.07% |
| Skill | 5 | 3 | 153 | 3.27% |
| ExecuteExtraTool→DynamicMCP | 5 | 1 | 16 | 31.25% |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_list_pages | 4 | 4 | 5 | 80.00% |
| ExecuteExtraTool→mcp__office-demo__anydoc | 4 | 2 | 8 | 50.00% |
| goal | 4 | 2 | 54 | 7.41% |
| SkillTool | 3 | 3 | 688 | 0.44% |
| Workflow | 3 | 2 | 21 | 14.29% |
| ExecuteExtraTool→goal | 3 | 1 | 79 | 3.80% |
| artifact | 2 | 2 | 29 | 6.90% |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | 2 | 2 | 2 | 100.00% |
| ExecuteExtraTool→LSP | 2 | 1 | 4 | 50.00% |
| HashlineEdit | 2 | 1 | 5 | 40.00% |
| _compact_note | 1 | 1 | 1 | 100.00% |
| ask_user | 1 | 1 | 1 | 100.00% |
| Delete | 1 | 1 | 1 | 100.00% |
| Edi | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→artifact | 1 | 1 | 11 | 9.09% |
| ExecuteExtraTool→gen-image | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp__gosqlx__format_sql | 1 | 1 | 2 | 50.00% |
| ExecuteExtraTool→mcp__gosqlx__parse_sql | 1 | 1 | 2 | 50.00% |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_logs | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp__openship__get_system_servers | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_create_page | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp_read_resource | 1 | 1 | 10 | 10.00% |
| ExecuteExtraTool→run_code | 1 | 1 | 1 | 100.00% |
| Export | 1 | 1 | 1 | 100.00% |
| file_path | 1 | 1 | 1 | 100.00% |
| Folder | 1 | 1 | 1 | 100.00% |
| Gash | 1 | 1 | 1 | 100.00% |
| mcp__plugin_hindsight-memory_hindsight__agent_knowledge_list_pages | 1 | 1 | 2 | 50.00% |
| mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | 1 | 1 | 1 | 100.00% |
| SearchExtraTool | 1 | 1 | 1 | 100.00% |
| Shell | 1 | 1 | 36 | 2.78% |
| Think | 1 | 1 | 1 | 100.00% |
| todo_write | 1 | 1 | 1 | 100.00% |
| UpdateGoal | 1 | 1 | 1 | 100.00% |

## Pairing

- paired=183364/183375 (99.99%)
- known state=183364/183364 (100.00%)
- missing=0, orphan=15, duplicate calls=0, duplicate results=0

## Result bytes

- count=183379, p50=669, p95=10490, giant=111, error_bytes=1223514

## Rules

| rule | kind | threshold | numerator | denominator | rate | next verification |
| --- | --- | --- | ---: | ---: | ---: | --- |
| pairing-quality | quality | paired results / normalized tool calls; known result state / paired results | 183364 | 183375 | 99.99% | Inspect missing and orphan result evidence before using error rates for behavior claims. |
| known-result-state | quality | paired results with explicit success or error state / paired results | 183364 | 183364 | 100.00% | Review unknown result states and source serialization before treating error rates as complete. |
| explicit-failure | candidate | the same labeled tool produces at least 2 consecutive explicit errors within a thread | 296 | 183364 | 0.16% | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| repeated-call | candidate | same canonical call and arguments occur at least 3 times consecutively within a thread | 78 | 183375 | 0.04% | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| large-tool-output | observation | UTF-8 result content is at least 100000 bytes | 111 | 183379 | 0.06% | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Candidates

| rule | count | denominator | evidence IDs | next verification |
| --- | ---: | ---: | ---: | --- |
| explicit-failure | 296 | 183364 | 8 | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| repeated-call | 78 | 183375 | 8 | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| large-tool-output | 111 | 183379 | 8 | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Capabilities

- tool_errors: **available** — paired normalized tool results with explicit error state
- repeated_calls: **available** — same labeled tool and stable canonical arguments within a thread
- result_bytes: **available** — UTF-8 byte counts from normalized tool results
- token_usage: **unavailable** — no stable token source in the analysis contract
- latency: **unavailable** — persisted event timestamps are not a stable execution duration source
- completion: **unavailable** — completion requires an explicit outcome label or human evidence
