# Inspect 数据质量

- schema: inspect.v1
- snapshot fingerprint: `sha256:c6710fef10e4fb99486c7e69210c2ff45ca0c945ede2c795113afebb83edca32`
- coverage boundary: created_min=2026-06-05T12:58:04.409637+00:00, updated_max=2026-09-13T00:28:33.896041+00:00
- schema metadata: user_version=3, tables=execution_runs,messages,projects,session_bindings,thread_goals,threads,workspaces
- 口径：全库质量检查；roots = `parent_thread_id IS NULL`，children = 有 parent 的线程；消息行按其持久化 thread 归属，snapshot inherited 单独计数；excluded 保留并独立计数。
- 默认报告不包含原始消息、路径或参数；evidence 仅列本机可查证 ID。

## Scope

- threads: total=10909, roots=4586, children=6323, hidden=6323
- messages: persisted_total=564115, own_rows=564115, inherited_rows=0, orphan_rows=0, snapshot_inherited=1617, snapshot_errors=0
- roles: {"user":23349,"assistant":223353,"tool":315266,"system":2010,"system_reminder":137}

## Canonical records

- excluded=59362, included=504753, truncated=25134, projection=13512

## Quality checks

| id | status | numerator | denominator | rate | reason |
| --- | --- | ---: | ---: | ---: | --- |
| schema.required_tables | pass | 2 | 2 | 100.00% |  |
| schema.required_columns | pass | 10 | 10 | 100.00% |  |
| messages.parse_fail | pass | 0 | 564115 | 0.00% |  |
| messages.normalization_issues | warn | 24 | 564115 | 0.00% | {"conflictingToolCall":14,"tool_call_missing_id_or_name":5,"tool_message_missing_id":5} |
| messages.count_reconciliation | pass | 0 | 10909 | 0.00% |  |
| messages.orphan_rows | pass | 0 | 564115 | 0.00% |  |
| messages.snapshot_inherited | pass | 0 | 10909 | 0.00% |  |
| tool_results.pairing | warn | 315257 | 315261 | 100.00% | orphans=4; unmatched_uses=1 |
| format.dual_write | warn | 113729 | 223353 | 50.92% |  |
| format.ambiguous_tool_use | warn | 5 | 315263 | 0.00% |  |
| messages.role_contract | pass | 0 | 564115 | 0.00% | role_mismatch=0; unknown_role=0 |

## Tool facts

- tool uses=315258, duplicate candidates=161048, tool results=315261, errors=6097
- raw tool rows=315266, rejected rows=5, rejected error rows=5, unknown error state=0
- paired results=315257, orphan results=4, unmatched uses=1

| tool | uses |
| --- | ---: |
| Read | 110615 |
| Bash | 69060 |
| Grep | 51913 |
| Edit | 41794 |
| Glob | 11181 |
| TodoWrite | 10396 |
| Agent | 5269 |
| Write | 5142 |
| folder_operations | 2665 |
| AskUserQuestion | 1585 |
| AgentResult | 1050 |
| ExecuteExtraTool | 856 |
| WebFetch | 850 |
| SkillTool | 688 |
| LineEdit | 538 |
| WebSearch | 507 |
| SearchExtraTools | 380 |
| SandboxWrite | 205 |
| Skill | 153 |
| Task | 56 |
| goal | 54 |
| run_code | 52 |
| DiscoverSkillsTool | 42 |
| HashlineEdit | 37 |
| Shell | 36 |
| artifact | 29 |
| LSP | 26 |
| Workflow | 21 |
| StrReplace | 19 |
| mcp__sentry__execute_sentry_tool | 5 |
| mcp__official-apps-fixture__get-time | 5 |
| Explore | 4 |
| mcp__context7__resolve-library-id | 2 |
| cron_list | 2 |
| Edi | 2 |
| mcp__plugin_hindsight-memory_hindsight__agent_knowledge_list_pages | 2 |
| Read&lt;/arg_value&gt; | 1 |
| mcp__sentry__find_organizations | 1 |
| UpdateGoal | 1 |
| Export | 1 |
| GreP | 1 |
| todo_write | 1 |
| glob | 1 |
| file_path | 1 |
| Folder | 1 |
| Think | 1 |
| ask_user | 1 |
| _compact_note | 1 |
| SearchExtraTool | 1 |
| Invoke | 1 |
| Gash | 1 |
| Delete | 1 |
| mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | 1 |

## Capabilities

- normalized_messages: **warn** — 0 JSON parse failure(s); normalization issues={"conflictingToolCall":14,"tool_call_missing_id_or_name":5,"tool_message_missing_id":5}
- tool_result_error_rate: **unavailable** — error rate requires paired tool results
- token_usage: **unavailable** — no stable token usage source is included in inspect
- user_satisfaction: **unavailable** — requires explicit human evidence

Evidence IDs retained: 27
