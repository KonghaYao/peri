Run a command in a fresh, non-interactive shell (Bash on Unix/macOS, PowerShell on Windows).
Usage: Cwd and shell state do not carry over. Prefer dedicated tools for file operations. Quote paths, verify destructive targets, and do not pipe downloads into a shell unless explicitly requested.

- Foreground defaults to 15000ms and caps at 120000ms. A timeout can return a still-running task receipt; if promotion fails, termination is requested. Check the existing task before retrying; a receipt is not an exit status.
- Run long-lived services in the foreground within `run_in_background: true`; do not detach them yourself. Use the task ID to track the shell task; read logs if the receipt supplies them, otherwise wait for the completion notice. If available, use the host task monitor for status or cancellation. Use its monitor ID for monitor calls; do not assume the shell task ID is the monitor ID. Verify exit after cancellation.
- Inline output can omit the tail. Follow saved-output references with Read, including nested references. A saved preview may not be the original output.
