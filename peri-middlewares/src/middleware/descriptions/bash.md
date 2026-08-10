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
- For builds, installs, or tests that may exceed 15s, set a longer `timeout` value (e.g. `timeout: 300000` for 5 minutes). Only use `run_in_background: true` for truly long-running processes like dev servers or watchers that should keep running while you continue work.

Platform behavior:
- Windows: uses powershell -NoProfile -NoLogo -NonInteractive -Command to execute commands
- Unix/macOS: uses bash -c to execute commands
- On Unix, child processes run in their own process group; timeout/cancel kills the entire process group (shell and all descendants), so no orphaned children survive
- On Windows, timeout only terminates the PowerShell wrapper process tree via taskkill; the process group semantics of Unix do not apply
- The command's stdin is redirected to /dev/null: interactive commands (read, prompts, editors, stdio services waiting on stdin) fail fast with an EOF error instead of hanging until timeout. Do not rely on terminal input; provide input via pipes or files instead

Output handling:
- Output exceeding 2000 lines is truncated (head + tail preserved)
- Output exceeding 65000 bytes is truncated
- Non-zero exit codes are reported
- Both stdout and stderr are captured

Background mode (run_in_background: true):
- Returns immediately with a `task_id`, the process `pid` of the background shell, and log file paths for live output
- To stop the task, run another shell command with `kill <pid>` (use `kill -- -<pid>` to kill the whole process group including child processes)
- Read the stdout/stderr log files at any time (they append while the command runs); monitor status and output preview in the Tasks panel
- The full captured output also arrives via a completion notification when the task finishes