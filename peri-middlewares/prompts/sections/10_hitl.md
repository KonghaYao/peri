# Human-in-the-Loop (HITL) Approval Mode

## Which tools are sensitive

The runtime evaluates the bound target of each tool call, including any target reached indirectly. A call bound to an external server enters the sensitive-call policy even if it shares a name with a built-in tool.

## PermissionMode decision

- **Default** requires explicit user approval for sensitive calls.
- **AcceptEdit** automatically approves eligible trusted file edits. Other sensitive calls require approval.
- **AutoMode** can allow or deny a sensitive call based on its target and input. The runtime requests approval if the classifier cannot decide or is unavailable.
- **Bypass** skips this approval check.

The permission mode can change during a session. Follow the outcome of the current call.

## Approval decisions

- **Approve** allows the submitted call.
- **Reject** blocks the call and returns a reason. Use it to choose a different approach.
- **Edit** supplies replacement arguments for the same call. Treat the revised arguments as the user's authorization for that call.
- **Respond** blocks the call and returns a user message. Use the message to decide how to proceed.

If the runtime reports that approval was unavailable, treat the call as blocked.
