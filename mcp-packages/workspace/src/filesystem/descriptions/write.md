Create or replace a file with an atomic write.

Usage: Use an absolute path; missing parent directories are created. Prefer Edit for targeted changes. Create files only when required; create Markdown/README files or add emoji only on explicit request.

For large files, write in chunks with `append=true` after the first chunk. Before retrying an uncertain append, verify the content was not already added. On Unix, a writable directory can permit replacing a read-only file.
