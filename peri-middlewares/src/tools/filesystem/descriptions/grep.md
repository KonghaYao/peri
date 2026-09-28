A powerful search tool built on ripgrep. Supports full regex syntax (e.g. "log.*Error", "function\s+\w+"). Filter files with glob parameter (e.g. "*.js", "*.{ts,tsx}") or type parameter (e.g. "js", "py", "rust", "go"). Use output_mode to control result format.

Usage:
- Always provide pattern parameter
- Use glob parameter for file type filtering (e.g. "*.js", "*.{ts,tsx}")
- Use type parameter for language-based filtering (e.g. "rust", "js", "py")
- Directory searches skip hidden files/directories and honor `.ignore` even outside Git repositories. Git ignore rules (`.gitignore`, `.git/info/exclude`, and configured global Git ignores) apply when searching within a Git repository. Ancestor ignore rules may also apply.
- An explicit `path` naming a file bypasses those directory-walk exclusions, so a hidden or ignored file can be searched directly. `No matches found.` for a directory only describes the searched files; use Glob to locate hidden/ignored files, then pass the specific file path here.
- Supports full regex syntax — literal braces need escaping (use \{\} to find interface{} in Go code)
- Output includes line numbers by default (use -n to disable)
- Search times out after 15 seconds; cancellation is checked between matches/context callbacks and at file boundaries. Blocking work already in progress may take longer to stop — use more specific patterns for large codebases.
- Default head_limit is 250 output lines (matched + context lines); exactly N matches are NOT marked truncated, only outputs beyond N are truncated and persisted
- In files_with_matches / count / files_without_matches modes, head_limit limits the number of files listed (each file counts as one output line)
- Lines longer than 1000 bytes are truncated at a UTF-8-safe boundary with a "… [line truncated]" marker
- Inline content has a 20000-byte budget, excluding notices. Reaching a line or byte budget stops collection; only the collected output is saved to a temp file (use Read to view), and only its head is returned inline. Counts marked `collected; total unknown` are not the total matches in the search tree. `head_limit: 0` removes the line limit, not the byte limit.
- Use fixed_strings (-F) to search literal strings without regex interpretation
- Use invert_match (-v) to find lines that do NOT match the pattern
- Use whole_word (-w) to match whole words only
- Use multiline to match patterns spanning multiple lines
- Use max_depth to limit search directory depth

Output modes:
- "content": shows matching lines with line numbers (default)
- "files_with_matches": lists only file paths that contain matches
- "count": shows match counts per file
- "files_without_matches": lists only file paths that do NOT contain matches

Context control:
- -C: symmetric context lines before and after each match
- -A: context lines after each match (takes priority over -C)
- -B: context lines before each match (takes priority over -C)

When to use:
- Prefer Grep over shell commands like grep or rg for content search
- Use Glob for file name search, Grep for content search
- For open-ended searches, start with the most specific query and broaden if needed
