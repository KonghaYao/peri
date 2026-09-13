# Agent behavior report

- schema: report.v1
- source fingerprint: c22d493aab76d24e24925b6352b5137cf5b3535563b6aa65c8d73904834da1e1
- normalizer: peri-normalizer-v2; metrics: agent-metrics-v1
- filters: scope=roots, include_hidden=false, since=2026-09-01T00:00:00.000Z, until=2026-09-13T00:00:00.000Z
- 默认输出只包含统计、规则和 bounded evidence refs，不包含原始消息、参数或路径。

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
| repeated-call | candidate | same canonical call and arguments occur at least 3 times consecutively within a thread | 2 | 19891 | 0.01% | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| explicit-failure | candidate | the same labeled tool produces at least 2 consecutive explicit errors within a thread | 28 | 19891 | 0.14% | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| large-tool-output | observation | UTF-8 result content is at least 100000 bytes | 1 | 19891 | 0.01% | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Candidates

| rule | count | denominator | evidence refs | next verification |
| --- | ---: | ---: | --- | --- |
| explicit-failure | 28 | 19891 | [report.json](report.json) :: 01a05fb3-1aaa-7aa0-af14-26ccd444eb92/01a05fc9-b93c-74c1-985e-64354be17a64/tool_118e3d17-068b-4f37-bf30-7df0ceb81f8; [report.json](report.json) :: 01a05fd0-c143-7813-b7dc-79d65fdffdae/01a05fef-d434-7a41-b1e1-f6db90b93462/tool_5da940c7-3613-4a0d-bd1e-1afe1ea8eca; [report.json](report.json) :: 01a0600b-70a3-79a3-80d7-1db56cd6e63a/01a06010-8f4a-7d80-9b5f-fe46caf03cfd/tool_c576007e-7a19-48d8-a86a-1ba28144c7c; [report.json](report.json) :: 01a060aa-7bdf-7c13-9002-c05d2673c43d/01a060ce-aad5-7e42-aca8-20678c8c0bf3/tool_6249ddef-300b-43ff-b16e-866f5ea172e; [report.json](report.json) :: 01a060ca-6af4-7df1-82c4-0b4060f3f3e3/01a060d6-fc97-7750-9fad-27f37e213ada/tool_03905aae-eef4-43e6-8d7a-c218651845d; [report.json](report.json) :: 01a060e1-38d0-7a12-a6e3-bdbdd12ac5f6/01a06537-45f4-7410-8737-43e81e9ec4a5/tool_1ffebd22-a13f-4cfa-8888-d8b9b9bca43; [report.json](report.json) :: 01a060fd-efe9-7b83-9854-4b83f52c17d8/01a06120-bb44-7d83-baa0-a43db5e4ed5f/call_43ZTfYa9BJjgOLrrfwy5OIGw; [report.json](report.json) :: 01a06a64-bef0-7f40-a119-9f27987b86ca/01a06a7d-27fe-7b90-86ac-1398dcf5307d/call_uwpLiULaJJ1FBiOI2IStQw8p | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| repeated-call | 2 | 19891 | [report.json](report.json) :: 01a084d2-9b15-7162-8cf8-61724378efff/01a084d8-1c8b-70e3-8a0a-5936645f0fa7/call_J90BQW3EGVM8i7n6UGm5bpEK; [report.json](report.json) :: 01a0906e-2489-7702-8198-87eee6bcce6e/01a09303-556c-7760-af8a-d11eb43bbcf4/call_486b2d98f4584cc7ab71fa91c846f014 | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| large-tool-output | 1 | 19891 | [report.json](report.json) :: 01a07e87-74c5-7491-b028-0c57ee14e408/01a07e89-751e-7192-bee7-372640371692/call_sgJh0Jo8j9E8Vm5E99KELYUB | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Capabilities

- tool_errors: **available** — paired normalized tool results with explicit error state
- repeated_calls: **available** — same labeled tool and stable canonical arguments within a thread
- result_bytes: **available** — UTF-8 byte counts from normalized tool results
- token_usage: **unavailable** — no stable token source in the analysis contract
- latency: **unavailable** — persisted event timestamps are not a stable execution duration source
- completion: **unavailable** — completion requires an explicit outcome label or human evidence
