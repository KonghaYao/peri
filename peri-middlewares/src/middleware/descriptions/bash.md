Executes a given shell command and returns its output.

Usage:
- The working directory persists between commands, but shell state does not. The shell environment is initialized from the user's profile (bash or zsh)
- IMPORTANT: Avoid using this tool to run find, grep, cat, head, tail, sed, awk, or echo commands, unless explicitly instructed or after you have verified that a dedicated tool cannot accomplish your task
- Instead, use the appropriate dedicated tool which will provide a much better experience for the user:
  - File search: Use Glob (NOT find or ls)
  - Content search: Use Grep (NOT grep or rg)
  - Read files: Use Read (NOT cat/head/tail)
  - Edit files: Use Edit (NOT sed/awk)
  - Write files: Use Write (NOT echo/cat with redirect)
- You can specify an optional timeout in milliseconds (up to 600000ms / 10 minutes). Foreground commands default to 15000ms (15 seconds) to encourage efficient commands; background tasks (run_in_background: true) run until completion unless timeout is explicitly set. Set `timeout: 0` to disable the timeout entirely
- When issuing multiple commands, use && to chain them together rather than using separate tool calls if the commands depend on each other
- For long-running commands, consider setting an explicit timeout or using run_in_background: true

Platform behavior:
- Windows: uses powershell -NoProfile -NoLogo -NonInteractive -Command to execute commands
- Unix/macOS: uses bash -c to execute commands
- On Unix, child processes run in their own process group; timeout/cancel kills the entire process group (shell and all descendants), so no orphaned children survive
- On Windows, timeout only terminates the PowerShell wrapper process tree via taskkill; the process group semantics of Unix do not apply

Output handling:
- Output exceeding 2000 lines is truncated (head + tail preserved)
- Output exceeding 65000 bytes is truncated
- Non-zero exit codes are reported
- Both stdout and stderr are captured