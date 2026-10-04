Executes a given shell command and returns its output.

Usage:
- Each invocation starts a new shell in the tool's configured working directory. A `cd` in one invocation does not change the starting directory of later invocations; use `cd "path" && command` within the same invocation when needed. Shell variables and other shell state do not persist between invocations. Do not assume user profile files are loaded; see the platform-specific shell invocation below.
- IMPORTANT: Avoid using this tool to run find, grep, cat, head, tail, sed, awk, or echo commands, unless explicitly instructed or after you have verified that a dedicated tool cannot accomplish your task
- Instead, use the appropriate dedicated tool which will provide a much better experience for the user:
  - File search: Use Glob (NOT find or ls)
  - Content search: Use Grep (NOT grep or rg)
  - Read files: Use Read (NOT cat/head/tail)
  - Edit files: Use Edit (NOT sed/awk)
  - Write files: Use Write (NOT echo/cat with redirect)
- You can specify an optional timeout in milliseconds. The synchronous path is always bounded: it defaults to 15000ms (15 seconds) and is capped at 120000ms (2 minutes) — `timeout: 0` is treated as that maximum instead of disabling the timeout, so no request can produce an unbounded synchronous wait. Background tasks (run_in_background: true) run until completion: omitting `timeout` or setting `timeout: 0` leaves a background command without a timeout, and a positive `timeout` (up to 600000ms) requests termination when reached
- When issuing multiple commands, use && to chain them together rather than using separate tool calls if the commands depend on each other
- For builds, installs, or tests that may exceed 15s, set a longer `timeout` value up to the 120000ms foreground maximum. Anything that may need longer, and long-running processes like dev servers or watchers, belongs in `run_in_background: true` — a synchronous command that reaches its timeout is promoted to a background task instead of being killed (see Timeout behavior).

Timeout behavior:
- The synchronous path is always bounded: the effective timeout is clamped to at most 120000ms, and `timeout: 0` is treated as that maximum rather than disabling the timeout. There is no way to disable the timeout on the synchronous path.
- The same foreground deadline covers both shell exit and stdout/stderr draining. `nohup ... &` can leave descendants holding the output pipes after the shell exits; that wait also times out and follows the background-promotion behavior below.
- Foreground timeout returns a timeout error, but does not terminate the process: when background task registration is available and succeeds, the command is promoted to a background task and the tool returns a background receipt. The command has already run for at least the foreground deadline, so the receipt is not evidence that it just started, and output produced before promotion is not included in the receipt.
- If background task registration is unavailable or fails, foreground timeout requests process termination.
- For commands explicitly started with `run_in_background: true`, a positive `timeout` requests process termination when reached. Omitting `timeout` or setting it to `0` leaves that background command without a timeout.
- Read the returned process status before retrying. If it says the process is still running, do not start a replacement for the same work; wait for the completion reminder or use the host's task cancellation interface to stop it.

Platform behavior:
- Windows: uses powershell -NoProfile -NoLogo -NonInteractive -Command to execute commands
- Unix/macOS: uses bash -c to execute commands
- On Unix, child processes run in their own process group. When termination is requested, cleanup targets the process group; this does not apply to a foreground timeout that continues in the background.
- On Windows, shell cleanup commands use taskkill for the PowerShell process tree; Unix process-group semantics do not apply.
- The command's stdin is redirected to /dev/null: interactive commands (read, prompts, editors, stdio services waiting on stdin) fail fast with an EOF error instead of hanging until timeout. Do not rely on terminal input; provide input via pipes or files instead

Output handling:
- Inline model output has a 10000-character budget, including truncation notices and space reserved for execution status. Output above 9488 characters takes a head-only preview path, even when it is below 65000 bytes; do not assume the tail is included. The retained head is cut at a UTF-8-safe byte boundary, so multibyte text may retain fewer characters.
- When that character-budget path is not triggered, the internal formatter initially selects the first and last 1000 lines from output over 2000 lines. It also has a 65000-byte cap. A later MCP projection keeps only the first 2000 rendered lines, including formatting notices, and adds an `[MCP output truncated: ...]` marker. This can remove the tail selected by the internal formatter even for short-line output below the character budget; no inline preview guarantees the final output lines are present.
- Each truncation layer attempts to save its own input to a temporary file. An MCP `[Full output saved to ...]` path can therefore contain an already shortened Bash result, not the original command output. Use Read with offset/limit to inspect that file; if it contains another saved-output path, follow that inner reference to read the original captured output and its tail. Persistence failures are reported in the result.
- Non-zero exit codes are reported
- Both stdout and stderr are captured

Background mode (run_in_background: true):
- Returns a task receipt carrying the task id. The receipt is the only handle it carries: it does not include a pid or live log paths.
- The completion reminder is delivered to the session that started the task and carries the exit status plus output file references; use Read on those files to inspect the captured output. Reminders for the same terminal transition are delivered once.
- Run the service in the foreground inside this already-backgrounded shell: do not add `&`, use `Start-Job`, or detach it with `Start-Process` without `-Wait`.
- Stop a running task through the host's task cancellation interface; killing only a shell PID can leave the service and its output pipes alive. If cancellation is unavailable, the process may keep running until it exits on its own.
- Preserve cleanup errors and verify actual process exit. Do not hide a failed kill with `2>/dev/null` or report success merely because a trailing `echo cleaned` succeeded.
