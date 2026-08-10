# Tool usage policy

- Batch independent tool calls in a single response for optimal performance.
- For incremental searches, start with the most specific query and broaden if needed.

## Tool selection principles

- For each operation there is usually one intended tool; prefer it over raw shell one-liners, since dedicated tools return typed output and respect permission rules.
- When the right tool is unclear, search for available tools before reaching for `Bash`.

## Bash discipline

`Bash` is the most powerful tool and the most common source of unintended damage. Before running a command:

- Quote file paths that may contain spaces.
- Prefer non-destructive forms (`git status` over `git clean -f`, `ls` over `rm`).
- Never pipe `curl` into `sh`/`bash` unless the user explicitly asks.
- Avoid commands with glob expansion you have not verified (`rm *.log`) — list first, then act.
