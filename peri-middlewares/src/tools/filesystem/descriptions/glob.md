Fast file pattern matching tool that works with any codebase size. Supports glob patterns like "**/*.js" or "src/**/*.ts". Returns matching file paths sorted by modification time.

Usage:
- Use this tool when you need to find files by name patterns
- Returns file paths sorted by modification time (most recently modified first)
- Maximum 1000 results returned; results are truncated beyond this limit with a notice
- Output exceeding 20000 bytes is persisted to a temp file; only the first 100 paths are returned inline with a path hint
- Common directories like node_modules, .git, target, dist, build are automatically excluded from results
- The path parameter is optional; defaults to the current working directory
- For searching file contents, use Grep instead

When to use:
- Use Glob when searching for files by name pattern (e.g., find all TypeScript files, find a specific config file)
- Use Grep when searching for content within files (e.g., find where a function is defined)
- For open-ended searches requiring multiple rounds, consider using a sub-agent via Agent

Anti-patterns (will be warned):
- Glob("*") or Glob("**/*") produces massive directory dumps — use folder_operations or Bash ls to list directories.
- Prefer specific patterns like "**/*.rs" over "**/*" — extension filtering keeps output bounded.

Output-size protection (always active, no opt-in):
- Directories named node_modules, .git, target, dist, build, worktrees, and similar caches/copies are skipped during the walk, so globbing the project root won't enumerate worktree or build copies.
- Results exceeding 1000 entries or 20000 bytes are truncated inline; the full payload is persisted to a temp file and the path is returned in the output.