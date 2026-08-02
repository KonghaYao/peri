---
name: cron
description: >
  Scheduled tasks (cron). Use when the user asks for recurring, periodic, or
  scheduled tasks — reminders, periodic reports, or anything that should run at
  a fixed interval ("every day at 9am", "每 5 分钟", "every Monday").
  Also use when managing existing scheduled tasks via cron_list / cron_remove.
userInvocable: true
---

# Scheduled Tasks (Cron)

You have access to scheduled task tools (`cron_register`, `cron_list`, `cron_remove`) for registering recurring automated tasks using standard 5-field cron expressions (`minute hour day_of_month month day_of_week`).

## When to Use

- The user asks for a recurring/periodic task: reminders, periodic reports, status checks, polling.
- The user asks to manage existing scheduled tasks (list, remove).
- The user's phrasing implies "every X", "每天/每周/每月", "at 9am daily", etc.

Do NOT register a cron task just because the user mentions a time — only when they explicitly want it to repeat automatically.

## Tools

| Tool | Purpose |
| --- | --- |
| `cron_register(expression, prompt)` | Register a new scheduled task. `prompt` is the user message submitted when the task fires. |
| `cron_list()` | List all registered tasks with status, next fire time, and prompt. |
| `cron_remove(id)` | Remove a task by its ID (shown by `cron_list`). |

## Cron Expression Format

Standard 5-field cron expression:

```
minute hour day_of_month month day_of_week
```

Examples:

- `*/5 * * * *` — every 5 minutes
- `0 9 * * *` — every day at 09:00
- `0 9 * * 1-5` — weekdays at 09:00
- `30 8 1 * *` — the 1st of each month at 08:30

Use `cron_list` to verify the registered task and its next fire time after registering.

## Safety

`cron_register` schedules future agent turns that fire without further user confirmation — treat it like delegating execution authority. Before registering:

- Confirm the user explicitly asked for a recurring task. Do not register cron tasks speculatively or "to be helpful."
- Prefer prompts that read or report over prompts that write, delete, commit, or run destructive commands. A cron that fires `git push --force` overnight is a footgun.
- State the schedule and the exact prompt you are about to register, then wait for confirmation if there is any ambiguity.
- Avoid tight intervals (e.g. `* * * * *`) unless the user asked for them — they burn tokens fast and can flood the session.

In approval mode, `cron_register` always prompts the user before registering.

## Workflow

1. Confirm the user wants a recurring task, and pin down the schedule + what the prompt should do.
2. Translate the schedule into a 5-field cron expression.
3. Call `cron_register`, then `cron_list` to confirm the task and its next fire time.
