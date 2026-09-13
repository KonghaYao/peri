# Agent behavior report

- schema: report.v1
- source fingerprint: 8d16f95b9543e58bf900962c95e614a27a7fb72f5f88f503e88b502a22d5770f
- normalizer: peri-normalizer-v2; metrics: agent-metrics-v1
- filters: scope=roots, include_hidden=false, since=-, until=-
- 默认输出只包含统计、规则和 bounded evidence refs，不包含原始消息、参数或路径。

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
| repeated-call | candidate | same canonical call and arguments occur at least 3 times consecutively within a thread | 78 | 183375 | 0.04% | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| explicit-failure | candidate | the same labeled tool produces at least 2 consecutive explicit errors within a thread | 296 | 183364 | 0.16% | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| large-tool-output | observation | UTF-8 result content is at least 100000 bytes | 111 | 183379 | 0.06% | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Candidates

| rule | count | denominator | evidence refs | next verification |
| --- | ---: | ---: | --- | --- |
| explicit-failure | 296 | 183364 | [report.json](report.json) :: 019e9a4b-879e-79e3-b0a0-89c840fd01f0/019e9a6d-41c4-7c20-828f-1b8362556ecf/call_01_RYtrz9bokNtTqoDntOIb4772; [report.json](report.json) :: 019e9c74-33b0-7070-9919-ad428a44f7db/019e9c77-e903-7cb2-8706-008178935caa/call_00_zVen60QGvLqfK38x8gP57532; [report.json](report.json) :: 019e9cdc-d71c-7532-8c1c-ad3878b6d544/019e9cdf-10a9-7c21-90ad-34e286056a7b/call_02_d8hRiUWnQdxnuGyL7P2W1148; [report.json](report.json) :: 019e9d6a-ed8c-7093-ad3e-f276c36a8f67/019e9d6e-f98b-7011-9015-f62b0829bd72/call_04_Zr1TZsEcppxaw0V8yCH36483; [report.json](report.json) :: 019ea5e1-1015-7383-b2af-1d95b20de876/019ea5e9-2e78-7153-a9e8-bb6dee7c048a/call_2842056fe6ac4580acb99dba; [report.json](report.json) :: 019eab0a-9e5a-7cb1-9891-7059b7dc2b62/019eab0b-8eb9-7301-8df2-a3c53b338fe3/call_01_hnXwEHMsk9MYuqn9vJfB5659; [report.json](report.json) :: 019eab37-0cbd-7cb3-bc12-f1d40d64b870/019eab42-8811-7f31-b2bc-fc9e7924fc1f/call_01_yUenZy1u1YIfcQaypphE7781; [report.json](report.json) :: 019eab9c-64ed-70a3-8c9d-07d21ecc1945/019eabf2-ae43-7172-b6e0-0e7ad87818ec/tooluse_Iftr3FirFgIUVJeudRnNjO | Inspect both failed results and the next user or assistant message before attributing a strategy defect. |
| repeated-call | 78 | 183375 | [report.json](report.json) :: 019e9a9d-36e8-7622-beb4-16e7312899c3/019e9aa7-8c8d-70c1-99b4-ce18fb8612c0/call_fb048d6c45df4abeb2e27488; [report.json](report.json) :: 019e9b45-052d-7492-8692-1efc3fd573f8/019e9bdf-826b-7821-ad3b-a2fddc254897/call_a77f732074b54d69b0913b4c; [report.json](report.json) :: 019e9d6a-ed8c-7093-ad3e-f276c36a8f67/019e9d6f-26ba-7662-9884-9133f9a9bcc9/call_00_tSm3bi4fIEldNAH9E3t76565; [report.json](report.json) :: 019ea70a-e4e6-7811-ba4e-673ece2316c0/019ea72a-669c-7cd1-b2b1-4faf8e5f1851/call_9ee0b17b35334f0cbd0f5575; [report.json](report.json) :: 019ea70a-e4e6-7811-ba4e-673ece2316c0/019ea72d-5de9-7e93-a091-4cdbab81dbcf/call_928688d6e16d4e87af8e7416; [report.json](report.json) :: 019eb1f4-9113-7de2-bf38-bd1ee63fea39/019eb20e-08ca-7561-a2a7-7b51ff9aba16/call_4e9609e16d19410381001b4c; [report.json](report.json) :: 019eb1f4-9113-7de2-bf38-bd1ee63fea39/019eb22a-f65f-7890-a105-7fa117d3fee1/call_98438046d9c34f47ad7cf11a; [report.json](report.json) :: 019eb1f4-9113-7de2-bf38-bd1ee63fea39/019eb254-30c7-7233-9db4-026bea8f750d/call_a0ed721f01dd48ff828647a2 | Read the triggering thread's preceding and following messages to distinguish polling from no-progress repetition. |
| large-tool-output | 111 | 183379 | [report.json](report.json) :: 019e9a9d-36e8-7622-beb4-16e7312899c3/019e9abe-ee19-7ce0-a4c0-adf4ba2df059/call_9f1259c58d7f49db880986ce; [report.json](report.json) :: 019e9b4d-ed24-7141-92dc-0e9861049ad7/019e9b4e-28da-77a1-96a4-2693f5078dc2/call_7648c875808d4e86bd341a60; [report.json](report.json) :: 019e9b4d-ed24-7141-92dc-0e9861049ad7/019e9b4f-e50c-7ae2-8d73-1532e9861ae8/call_2a70cd76271e43058f32460e; [report.json](report.json) :: 019e9bec-cfc8-7ec3-99c7-e3db6539b11c/019e9bec-efde-7ef0-8c4a-2818b406f1ab/call_12f2857307024b0fb05bbeb2; [report.json](report.json) :: 019e9c74-33b0-7070-9919-ad428a44f7db/019e9c74-96f8-7770-9a84-b9e7f20504fa/call_00_JjxhMaQVEGFp90X1ogzX1737; [report.json](report.json) :: 019e9d88-7ead-7461-a76e-9c66f663b630/019e9d8c-8399-7803-880c-0e9d2ba2a368/call_00_AzFIk6a77rU3OdIvjSFh8472; [report.json](report.json) :: 019e9d94-f74b-7e00-b201-c63b7821451d/019e9fef-0316-7df0-bbd7-2477257967fb/call_00_gnBidAEaXJ6RZpyYzJW17106; [report.json](report.json) :: 019ea9ee-c586-72e0-b5d8-f0d551919c95/019eab13-926d-73c3-9221-530e472163d0/call_00_wKSbyoQCge2h5QCyvL6x9096 | Inspect the cited result in its surrounding conversation before drawing a conclusion. |

## Capabilities

- tool_errors: **available** — paired normalized tool results with explicit error state
- repeated_calls: **available** — same labeled tool and stable canonical arguments within a thread
- result_bytes: **available** — UTF-8 byte counts from normalized tool results
- token_usage: **unavailable** — no stable token source in the analysis contract
- latency: **unavailable** — persisted event timestamps are not a stable execution duration source
- completion: **unavailable** — completion requires an explicit outcome label or human evidence
