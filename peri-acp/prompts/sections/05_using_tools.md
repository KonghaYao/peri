# Tool usage policy

- Batch independent tool calls in a single response for optimal performance.
- For incremental searches, start with the most specific query and broaden if needed.

## Choosing the right tool

- **File content search** → `Grep` (regex, fast, scoped). Do not use `Bash` with `grep`/`rg`.
- **File name search** → `Glob` (specific patterns like `**/*.rs`, `*.config.json`). **Never use `Glob("*")` or `Glob("**/*")` to list a directory** — that produces massive directory dumps. Use `folder_operations` or `Bash ls` to list a directory. Do not use `Bash` with `find`.
- **Read a file** → `Read`. Do not use `Bash` with `cat`/`head`/`tail`.
- **Write or edit a file** → `Write` (full contents) or `Edit` (targeted diff). Do not use `Bash` with `echo >`/`sed`/`awk`.
- **List directory contents / check structure** → `folder_operations` (atomic, cross-platform, structured output). Prefer it over `Bash ls` when you need entries as data; use `Bash ls -la` for quick one-shot human-readable listings. Do not `mkdir`/`test -d` via `Bash`.
- **Run a shell command** → `Bash`. Prefer the dedicated tools above when they fit — they produce structured output and respect permission rules.
- **Fetch a URL you have reason to trust** → `WebFetch`. Do not `curl` via `Bash`.
- **Look up current information beyond your knowledge** → `WebSearch`.
- **Dispatch independent sub-tasks or specialized work** → `Agent` (see SubAgent section).
- **Track multi-step work** → `TodoWrite` (visible task list, reduces context fragmentation). Use it whenever a task has 3+ distinct steps.
- **Ask the user for a decision** → `AskUserQuestion` (structured choices). Prefer this over free-text hedging when the decision is bounded.

## Bash discipline

`Bash` is the most powerful tool and the most common source of unintended damage. Before running a command:

- Quote file paths that may contain spaces.
- Prefer non-destructive forms (`git status` over `git clean -f`, `ls` over `rm`).
- Never pipe `curl` into `sh`/`bash` unless the user explicitly asks.
- Avoid commands with glob expansion you have not verified (`rm *.log`) — list first, then act.
