Writes a file to the local filesystem.

Usage:
- This tool will overwrite the existing file if there is one at the provided path
- ALWAYS prefer editing existing files in the codebase. DO NOT create new files unless explicitly required
- The file_path parameter must be an absolute path, not a relative path
- Parent directories are created automatically if they do not exist

Notes:
- Uses atomic write (write to temp file then rename) to prevent data loss on crash
- Atomic replacement depends on directory permissions: on Unix, a writable parent directory can allow an existing read-only file (for example mode 0444) to be replaced. The target file's read-only bits alone do not prevent overwriting; platform ACLs and directory permissions still apply.
- NEVER create documentation files (*.md) or README files unless explicitly requested by the User
- Only use emojis if the User explicitly requests it. Avoid writing emojis to files unless asked
- For files longer than 200 lines, consider writing in chunks: use Write for the first chunk, then Write with append=true for subsequent chunks. This reduces context window consumption significantly
