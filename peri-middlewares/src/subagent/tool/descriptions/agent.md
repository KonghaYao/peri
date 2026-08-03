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
- **subagent_type is REQUIRED** unless fork=true. Specify an agent ID matching an existing agent definition file. Do NOT omit this parameter unless you intend to fork the current agent
- The sub-agent inherits the parent's tool set by default, excluding Agent itself (to prevent recursion)
- **Authorization boundary**: approving the `Agent` tool grants the sub-agent the right to execute its inherited tools. Sub-agents do NOT run per-tool HITL approval — internal tool calls (Bash, Write, Edit, WebFetch, MCP, ...) execute without further approval prompts. The transfer is single-level: sub-agents cannot recursively launch further sub-agents
- Agent definitions may restrict available tools via the tools and disallowedTools fields in frontmatter
- The sub-agent executes in isolated state — it cannot access the parent's message history or intermediate results

When to use:
- For tasks that benefit from independent context isolation (e.g., code review while working on a different feature)
- For tasks requiring specialized persona or behavior defined in agent configuration files
- For parallelizable sub-tasks that do not depend on each other's results
- When you need to break a complex task into smaller, independently executable pieces

Return format:
- If the sub-agent made tool calls, the result includes a summary of tools used followed by the final response
- If no tool calls were made, only the final response text is returned

Background execution (run_in_background: true):
- The sub-agent runs asynchronously in the background while the main agent continues
- Maximum 3 concurrent background tasks
- The main agent will be notified when the background task completes via a system message
- Use for long-running tasks that don't block the main workflow (e.g., code review, batch operations)
- Background tasks share the same working directory as the main agent