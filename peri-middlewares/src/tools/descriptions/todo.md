Maintain a todo list for complex multi-step tasks. Call this to create or update your todo list with the complete current state. Each call fully replaces the previous list.

Usage:
- Use this tool when working on complex, multi-step tasks that benefit from tracking progress
- Each call sends the COMPLETE todo list — this is a full replacement, not a partial update
- Include ALL items in every call, not just changed ones
- Mark items as "in_progress" when starting work on them, and "completed" when done
- Keep descriptions concise but specific enough to understand at a glance

When to use:
- Use for tasks with 3+ distinct steps that require tracking
- Use when the user explicitly asks for a plan or task breakdown
- Do NOT use for simple, single-step tasks
- Do NOT use for tasks that can be completed in a single tool call

Status values:
- "pending": Not yet started
- "in_progress": Currently being worked on
- "completed": Finished successfully
