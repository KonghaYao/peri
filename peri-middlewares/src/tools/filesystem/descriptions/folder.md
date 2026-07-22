Unified folder operations tool supporting create, list, and existence check.

Operations:
- "create": Creates a directory at the specified path. By default creates parent directories recursively (recursive: true). Use recursive: false to only create a single directory level
- "list": Lists the contents of a directory, showing files and subdirectories with sizes and modification dates. Output is truncated beyond 500 entries
- "exists": Checks whether a path exists and whether it is a directory or file
- "deep_scan": Recursively scans a directory tree and outputs entries in tree format with unicode box-drawing characters. Supports max_depth to control recursion depth (default 3, range 1-10). Common cache/build directories (node_modules, .git, target, dist, etc.) are automatically skipped. Output is truncated beyond 500 entries.

Usage:
- The folder_path parameter must be an absolute path, not a relative path
- You can call multiple tools in a single response. It is always better to check directory existence before creating or listing
- When creating a directory, the recursive parameter defaults to true, creating all necessary parent directories

Notes:
- List output shows entries with file size and modification date
- Directories are shown with a trailing / indicator
- For large directories (>500 entries), output is truncated with a summary count