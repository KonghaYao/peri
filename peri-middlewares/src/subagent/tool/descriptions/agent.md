Launch a sub-agent with an independent context to handle a specialized sub-task. The sub-agent executes based on the configuration defined in .claude/agents/{subagent_type}.md or .claude/agents/{subagent_type}/agent.md.

Fork mode (fork: true):
- Inherits the parent's frozen system prompt, a full history snapshot at launch time, and the parent's core tool set (Filesystem, Bash, Web, MCP)
- Does NOT inherit the Agent tool (prevents recursion) nor Cron / Workflow / LSP / Plugin extension tools; parent agent_overrides blocks do not enter the forked prompt
- The prompt is treated as a directive within the existing context, not a standalone briefing
- Do NOT re-explain background that is already in the conversation history
- Use for tasks that require context from the ongoing conversation (e.g., continuing a multi-file refactor)
- The forked agent follows a structured output format: Scope, Result, Key files, Files changed

Usage:
- Provide a clear, self-contained task description via the prompt parameter. The sub-agent has no access to the parent conversation history
- **subagent_type is REQUIRED for NEW sub-agents** unless fork=true. Specify an agent ID matching an existing agent definition file. Do NOT omit this parameter unless you intend to fork the current agent — **or resume** (when `resume_thread_id` is provided, `subagent_type` and `fork` are ignored: resume takes priority)
- The sub-agent inherits the parent's tool set by default, excluding Agent itself (to prevent recursion)
- **Authorization boundary**: approving the `Agent` tool grants the sub-agent the right to execute its inherited tools. Sub-agents do NOT run per-tool HITL approval — internal tool calls (Bash, Write, Edit, WebFetch, MCP, ...) execute without further approval prompts. The transfer is single-level: sub-agents cannot recursively launch further sub-agents
- Agent definitions may restrict available tools via the tools and disallowedTools fields in frontmatter
- The sub-agent executes in isolated state — it cannot access the parent's message history or intermediate results

Model selection (model):
- Optional; only applies to NEW defined-type sub-agents (subagent_type path, including background). Overrides the `model` declared in the agent definition frontmatter; when omitted, the definition's model is used as-is
- Available tiers: `inherit` (use the parent agent's model), `haiku` (fastest/cheapest, best for quick lookups and simple sub-tasks), `sonnet` (balanced default), `opus` (strongest reasoning), `fable` (flagship tier)
- Unknown tiers are rejected with an error — never silently ignored
- Does NOT apply to forks: `fork: true` always inherits the parent model, and `model` is ignored
- Does NOT apply to resume: `resume_thread_id` restores the original execution context, and `model` is ignored

When to use:
- For tasks that benefit from independent context isolation (e.g., code review while working on a different feature)
- For tasks requiring specialized persona or behavior defined in agent configuration files
- For parallelizable sub-tasks that do not depend on each other's results
- When you need to break a complex task into smaller, independently executable pieces
- **When an Agent call returns an interrupted/error message or a background notification contains `child_thread_id: xxx (resume with Agent(resume_thread_id: xxx))` and the task still needs to be completed, resume the execution with `Agent(resume_thread_id: xxx)` instead of launching a new sub-agent** — this avoids repeating work already done and losing side effects

Return format:
- If the sub-agent made tool calls, the result includes a summary of tools used followed by the final response
- If no tool calls were made, only the final response text is returned

Background execution (run_in_background: true):
- Runs the sub-agent asynchronously while the main agent continues immediately.
- Maximum 3 concurrent background tasks.
- The main agent will be notified when the task completes via a system message.
- **Only use when you genuinely need to continue working while the sub-agent runs** (e.g., offloading a long-running code review while you proceed with other edits). For most cases, run sub-agents synchronously to integrate their results immediately.

Send or resume (resume_thread_id):
- Use the existing `resume_thread_id` and `prompt` fields to continue interacting with a sub-agent. The target's current execution state determines the behavior; read the returned `action` to distinguish sending from resuming.
- **Active background sub-agent in this session:** a non-empty `prompt` is queued as Info and the tool immediately returns `action: send`, `status: queued`, and the target IDs. It does not create or resume execution, interrupt an in-flight model/tool call, or trigger an extra model call. Info enters the transcript at the next Receive; the agent may finish before the model sees it. Queued does not mean read or durably saved. `run_in_background` is ignored for sending.
- **Non-active thread:** the persisted transcript is replayed and execution resumes, returning `action: resume`. `prompt` is optional; omitting it implicitly continues the task. `run_in_background: true` selects background execution for this resume.
- Active threads without a live background receiver in this session (including crash leftovers or another session's tasks) return an error. They are not silently resumed or recreated.
- Both paths take priority over `subagent_type` and `fork`; those fields are ignored. Sending requires no additional tool or parameters.
- **Common failures**: (1) passing `subagent_type` or `fork` together with `resume_thread_id` — harmless, they are ignored; resume always wins. (2) `thread not found` / `invalid thread id` → the id is stale or malformed; use the `child_thread_id` exactly as returned in the interrupted/error/bg notification text.
