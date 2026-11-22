Read a local file with 1-based line numbers.

Usage: Omit offset unless the start line was observed or provided by the user. Never guess or estimate an offset. Continue from the last line number actually shown plus 1.

Error handling: Line offsets cannot recover text truncated within a single line or bypass the 32 MB file limit. Use folder_operations for directories. Supported images up to 10 MB return image content.
