# Comparison report

- compatibility: compatible
- window comparison: descriptive-different-creation-window
- baseline source: 99715df047e6092194662cb6dd53e84875888ebc17f27b5a421407e42d72b6f0
- candidate source: c22d493aab76d24e24925b6352b5137cf5b3535563b6aa65c8d73904834da1e1
- baseline filters: scope=roots, include_hidden=false, since=2026-08-20T00:00:00.000Z, until=2026-09-01T00:00:00.000Z
- candidate filters: scope=roots, include_hidden=false, since=2026-09-01T00:00:00.000Z, until=2026-09-13T00:00:00.000Z

## Samples

- baseline threads/messages/calls: 269/35091/21427
- candidate threads/messages/calls: 204/33961/19891
- paired known results baseline/candidate: 21427/19891

## Rule rates

| rule | threshold | baseline n/d | candidate n/d | baseline rate | candidate rate | delta |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| pairing-quality | paired results / normalized tool calls; known result state / paired results | 21427/21427 | 19891/19891 | 100.00% | 100.00% | 0.00pp |
| known-result-state | paired results with explicit success or error state / paired results | 21427/21427 | 19891/19891 | 100.00% | 100.00% | 0.00pp |
| repeated-call | same canonical call and arguments occur at least 3 times consecutively within a thread | 0/21427 | 2/19891 | 0.00% | 0.01% | 0.01pp |
| explicit-failure | the same labeled tool produces at least 2 consecutive explicit errors within a thread | 41/21427 | 28/19891 | 0.19% | 0.14% | -0.05pp |
| large-tool-output | UTF-8 result content is at least 100000 bytes | 3/21427 | 1/19891 | 0.01% | 0.01% | -0.01pp |

## Tool error rates

| tool | baseline errors/known | candidate errors/known | baseline rate | candidate rate | delta |
| --- | ---: | ---: | ---: | ---: | ---: |
| Read | 157/7449 | 69/5298 | 2.11% | 1.30% | -0.81pp |
| Edit | 86/3353 | 84/3667 | 2.56% | 2.29% | -0.27pp |
| Grep | 47/4599 | 13/3656 | 1.02% | 0.36% | -0.67pp |
| Agent | 37/365 | 41/347 | 10.14% | 11.82% | 1.68pp |
| run_code | 29/52 | null/null | 55.77% | null | null |
| Bash | 16/3318 | 21/4235 | 0.48% | 0.50% | 0.01pp |
| WebFetch | 13/55 | 14/66 | 23.64% | 21.21% | -2.42pp |
| ExecuteExtraTool→RunPtcCode | 12/24 | 1/20 | 50.00% | 5.00% | -45.00pp |
| AskUserQuestion | 10/77 | 9/91 | 12.99% | 9.89% | -3.10pp |
| folder_operations | 8/190 | 2/288 | 4.21% | 0.69% | -3.52pp |
| ExecuteExtraTool→DynamicMCP | 5/16 | null/null | 31.25% | null | null |
| ExecuteExtraTool→Workflow | 4/39 | 24/61 | 10.26% | 39.34% | 29.09pp |
| WebSearch | 4/68 | 2/54 | 5.88% | 3.70% | -2.18pp |
| Glob | 4/574 | 7/589 | 0.70% | 1.19% | 0.49pp |
| Write | 2/218 | 6/260 | 0.92% | 2.31% | 1.39pp |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_list_pages | 1/1 | 0/1 | 100.00% | 0.00% | -100.00pp |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | 1/1 | 1/1 | 100.00% | 100.00% | 0.00pp |
| ExecuteExtraTool→run_code | 1/1 | null/null | 100.00% | null | null |
| artifact | 0/9 | 0/1 | 0.00% | 0.00% | 0.00pp |
| DiscoverSkillsTool | 0/14 | 0/9 | 0.00% | 0.00% | 0.00pp |
| ExecuteExtraTool→DiscoverMCP | 0/9 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__official-everything__echo | 0/2 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__plugin_hindsight-memory_hindsight__agent_knowledge_get_current_bank | 0/3 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__remote-agent__bash | 0/2 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__remote-agent__edit | 0/1 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__remote-agent__glob | 0/3 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__remote-agent__grep | 0/2 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__remote-agent__read | 0/2 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp__remote-agent__write | 0/1 | null/null | 0.00% | null | null |
| ExecuteExtraTool→mcp_read_resource | 0/2 | null/null | 0.00% | null | null |
| mcp__official-apps-fixture__get-time | 0/5 | null/null | 0.00% | null | null |
| SearchExtraTools | 0/59 | 0/34 | 0.00% | 0.00% | 0.00pp |
| SkillTool | 0/116 | 1/118 | 0.00% | 0.85% | 0.85pp |
| TodoWrite | 0/797 | 0/1006 | 0.00% | 0.00% | 0.00pp |
| StrReplace | null/null | 19/19 | null | 100.00% | null |
| Workflow | null/null | 2/8 | null | 25.00% | null |
| Delete | null/null | 1/1 | null | 100.00% | null |
| ExecuteExtraTool | null/null | 1/1 | null | 100.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_logs | null/null | 1/1 | null | 100.00% | null |
| ExecuteExtraTool→mcp__openship__get_system_servers | null/null | 1/1 | null | 100.00% | null |
| Gash | null/null | 1/1 | null | 100.00% | null |
| mcp__plugin_hindsight-memory_hindsight__agent_knowledge_recall | null/null | 1/1 | null | 100.00% | null |
| Shell | null/null | 1/36 | null | 2.78% | null |
| ExecuteExtraTool→cron_list | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→cron_register | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_deployments | null/null | 0/2 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_github_repos_by_owner_by_repo | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_github_repos_by_owner_by_repo_branches | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_github_repos_by_owner_by_repo_detect | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_github_status | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_issues | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects | null/null | 0/2 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_incidents | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_info | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_pending_actions | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_resources | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_services | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_projects_by_id_services_containers | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__get_settings | null/null | 0/1 | null | 0.00% | null |
| ExecuteExtraTool→mcp__openship__post_projects | null/null | 0/1 | null | 0.00% | null |

## Limitations

- Creation windows differ; differences describe two task populations and do not establish a causal improvement.
- Different thread composition, message format, model, or data quality can explain observed differences.
- Missing rules and missing tools remain null; absence is not interpreted as zero errors or zero calls.
