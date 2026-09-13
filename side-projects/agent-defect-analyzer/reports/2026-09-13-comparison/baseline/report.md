# Agent behavior report

- schema: report.v1
- source fingerprint: 99715df047e6092194662cb6dd53e84875888ebc17f27b5a421407e42d72b6f0
- normalizer: peri-normalizer-v2; metrics: agent-metrics-v1
- filters: scope=roots, include_hidden=false, since=2026-08-20T00:00:00.000Z, until=2026-09-01T00:00:00.000Z
- 默认输出只包含统计、规则和 bounded evidence refs，不包含原始消息、参数或路径。

## Totals

- threads=269, messages=35091, calls=21427
- explicit_errors=437, unknown_error_results=0, parse_issues=0

## Top tool errors

| tool | errors | affected threads | known result denominator | error rate |
| --- | ---: | ---: | ---: | ---: |
| Read | 157 | 54 | 7449 | 2.11% |
| Edit | 86 | 31 | 3353 | 2.56% |
| Grep | 47 | 23 | 4599 | 1.02% |
| Agent | 37 | 20 | 365 | 10.14% |
| run_code | 29 | 2 | 52 | 55.77% |
| Bash | 16 | 10 | 3318 | 0.48% |
| WebFetch | 13 | 8 | 55 | 23.64% |
| ExecuteExtraTool→RunPtcCode | 12 | 1 | 24 | 50.00% |
| AskUserQuestion | 10 | 8 | 77 | 12.99% |
| folder_operations | 8 | 7 | 190 | 4.21% |
| ExecuteExtraTool→DynamicMCP | 5 | 1 | 16 | 31.25% |
| ExecuteExtraTool→Workflow | 4 | 4 | 39 | 10.26% |
| WebSearch | 4 | 3 | 68 | 5.88% |
| Glob | 4 | 2 | 574 | 0.70% |
| Write | 2 | 2 | 218 | 0.92% |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_list_pages | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | 1 | 1 | 1 | 100.00% |
| ExecuteExtraTool→run_code | 1 | 1 | 1 | 100.00% |

## Pairing

- paired=21427/21427 (100.00%)
- known state=21427/21427 (100.00%)
- missing=0, orphan=0, duplicate calls=0, duplicate results=0

## Result bytes

- count=21427, p50=2064, p95=12535, giant=3, error_bytes=100787

## Rules

| rule | kind | threshold | numerator | denominator | rate | next verification |
| --- | --- | --- | ---: | ---: | ---: | --- |
| pairing-quality | quality | paired results / normalized tool calls; known result state / paired results | 21427 | 21427 | 100.00% | Inspect missing and orphan result evidence before using error rates for behavior claims. |
| known-result-state | quality | paired results with explicit success or error state / paired results | 21427 | 21427 | 100.00% | Review unknown result states and source serialization before treating error rates as complete. |
| repeated-call | candidate | same canonical call and arguments occur at least 3 times consecutively within a thread | 0 | 21427 | 0.00% | No qualifying observation in this report; verify the denominator and inspect a seeded sample before concluding absence. |
| explicit-failure | candidate | the same labeled tool produces at least 2 consecutive explicit errors within a thread | 41 | 21427 | 0.19% | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| large-tool-output | observation | UTF-8 result content is at least 100000 bytes | 3 | 21427 | 0.01% | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Candidates

| rule | count | denominator | evidence refs | next verification |
| --- | ---: | ---: | --- | --- |
| explicit-failure | 41 | 21427 | [report.json](report.json) :: 01a01d54-ee4d-7f51-8060-e64345a0967d/01a01d62-2646-7052-a879-fb462eded4e1/call_WHDL96a3UMEjC9sGhfCFhlxG; [report.json](report.json) :: 01a01df9-99f8-7b43-9f86-ed00ab58f035/01a01e37-69aa-7e83-a4e2-2f4a1d91c252/call_hMlFbPk8hfXhIxyKfWt2acrg; [report.json](report.json) :: 01a01e29-933f-7a13-998b-3febd5466738/01a01e36-7761-7152-9b57-7daaf4769262/call_gMR0iyvsrIf8z8utiYgD7atd; [report.json](report.json) :: 01a021df-3295-75f0-bb7b-dcad79d5a4a3/01a0226b-304f-7f93-9062-1af57c1d0329/call_g2xVE4tBETrYnl1JG7DvXgqx; [report.json](report.json) :: 01a021f5-459f-74d2-9f99-b53544607e74/01a021f6-81bf-79f3-ba38-a33e03eb4702/call_gpz0XzF2PR0cqWtwH7NbI9fy; [report.json](report.json) :: 01a021f9-81cb-7da1-8231-ccc89ac4808a/01a02274-cb29-7cd0-a70d-77708fd861a2/call_8ylwiam4uGZAcUJ8D9UXsL5Q; [report.json](report.json) :: 01a0220e-6033-7202-b47a-c23a39f6ec08/01a02235-a6de-7580-8f8c-89c9542a4322/call_wNMTYcXt3aBGeamaIi5xPvox; [report.json](report.json) :: 01a02220-12c2-7590-931d-00830a05c75d/01a02322-38a4-7753-bf98-e9710e5764bf/call_StFyliwFMKlxqTglHHY8No3u | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| large-tool-output | 3 | 21427 | [report.json](report.json) :: 01a02a31-1054-7330-97f5-0e8e2daf9c3e/01a02a35-023a-7130-b14a-dce79f3d7dc3/call_o5VADmhlLWbqWsBiaQiQp450; [report.json](report.json) :: 01a02c33-498f-73d0-9497-6f7e227094c7/01a02c38-b279-73a1-a9f7-c1a84b7019d6/call_2GnKu0iLrHm6b7zTo30ocJSP; [report.json](report.json) :: 01a02cb2-ee5f-7c50-9e32-570d0f6dc782/01a02cb6-032d-7312-b1b3-1088d6271ce5/call_OFpJV87qKoDLq0oH04RPXQpJ | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Capabilities

- tool_errors: **available** — paired normalized tool results with explicit error state
- repeated_calls: **available** — same labeled tool and stable canonical arguments within a thread
- result_bytes: **available** — UTF-8 byte counts from normalized tool results
- token_usage: **unavailable** — no stable token source in the analysis contract
- latency: **unavailable** — persisted event timestamps are not a stable execution duration source
- completion: **unavailable** — completion requires an explicit outcome label or human evidence
