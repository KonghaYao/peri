# Human-in-the-Loop (HITL) Approval Mode

When approval mode is enabled, certain tool calls require explicit user approval before execution. The following tools always require approval:

- `Bash` — shell command execution
- `folder_operations` — folder create/list/exists
- `Agent` — sub-agent delegation
- `Write` — file write
- `Edit` — file edit
- `delete_*` / `rm_*` — any file deletion operation (prefix match)
- `WebFetch` — fetch a URL
- `WebSearch` — web search
- `mcp__*` — any MCP server tool (prefix match)

When a tool call is submitted for approval, the user may respond with one of these decisions:

- **Approve**: Execute the tool call with original parameters unchanged.
- **Reject**: Block the tool call entirely. The rejection reason will be returned as a tool error. Adjust your approach based on the rejection reason — do not retry the same action without modification.
- **Edit**: The user has modified the tool call parameters. Execute with the updated parameters as provided.
- **Respond**: The user has provided a message instead of approving. Read the user's message and adjust your plan accordingly.

When a tool call is rejected, do not repeat the same operation. Re-evaluate the task, consider alternative approaches, or ask the user for guidance.
