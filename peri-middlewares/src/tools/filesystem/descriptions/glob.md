Fast file pattern matching tool that works with any codebase size. Supports glob patterns like "**/*.js" or "src/**/*.ts". Returns matching file paths sorted by modification time.

Usage:
- Use this tool when you need to find files by name patterns
- Returns file paths sorted by modification time (most recently modified first)
- Maximum 1000 results returned; collection stops once the limit is exceeded and results are truncated with a notice. The shown 1000 are the newest among the collected matches (walk order), not necessarily the globally newest files in the tree
- If collection stays within 1000 matches but the paths exceed 20000 bytes, they are persisted to a temp file; only the first 100 paths are returned inline with a path hint. The result-count limit takes priority when both limits are exceeded.
- Common directories like node_modules, .git, target, dist, build are automatically excluded from results
- Unlike Grep directory search, this tool does not read `.gitignore` or `.ignore`. Hidden files and directories are included unless their directory name is on the exclusion list.
- The path parameter is optional; defaults to the current working directory
- An explicitly supplied `path` is canonicalized when possible: a symlink used as the search root is resolved and its target is searched, with canonical paths returned. Symbolic links encountered inside the walk are skipped, including links pointing outside that resolved root.
- Searches taking longer than 15 seconds time out with an error; use a more specific pattern or an explicit path
- For searching file contents, use Grep instead

When to use:
- Use Glob when searching for files by name pattern (e.g., find all TypeScript files, find a specific config file)
- Use Grep when searching for content within files (e.g., find where a function is defined)
- For open-ended searches requiring multiple rounds, consider using a sub-agent via Agent

Anti-patterns (will be warned):
- Glob("*") or Glob("**/*") produces massive directory dumps — use folder_operations or Bash ls to list directories.
- Prefer specific patterns like "**/*.rs" over "**/*" — extension filtering keeps output bounded.

Output-size protection (always active, no opt-in):
- Directories named node_modules, .git, target, dist, build, worktrees, and similar caches/copies are skipped during the walk, so globbing the project root won't enumerate worktree or build copies. The blacklist applies by directory name at every level, including the search root itself.
- Results exceeding 1000 entries or 20000 bytes are truncated inline; the collected output is persisted to a temp file and the path is returned in the output. Collection stops at the result limit, so the persisted file may not contain every match in the tree.
- Note: wildcards are matched with the glob crate defaults, so `*`/`?` also cross `/` (e.g. `*.rs` matches nested files too). Use a directory prefix such as `src/**/*.rs` to scope the walk; the walk only descends into directories matching the pattern's literal prefix.
