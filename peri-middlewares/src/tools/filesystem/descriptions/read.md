Reads a file from the local filesystem. You can access any file directly by using this tool.
Assume this tool is able to read all files on the machine. If the User provides a path to a file assume that path is valid. It is okay to read a file that does not exist; an error will be returned.

Usage:
- The file_path parameter must be an absolute path, not a relative path
- By default, it reads up to 2000 lines starting from the beginning of the file
- Omit offset by default. Set offset only when its 1-based line number is already known from Read/Grep output or was explicitly provided by the user
- Never guess or estimate an offset, and never use a large offset to probe the end of a file. If the file length is unknown, read without offset
- To continue after a partial or truncated result, use the last line number actually shown plus 1. Do not calculate the next offset from limit or an assumed file length
- Any lines longer than 65536 characters will be truncated; the result reports the original and retained character counts before the line content
- Results exceeding 5000 bytes are truncated with the original byte count, and the complete numbered output is persisted to a temporary file
- Results are returned using cat -n format, with line numbers starting at 1
- This tool reads files from the local filesystem; it cannot handle URLs
- You can call multiple tools in a single response. It is always better to speculatively read multiple files before making edits
- You should prefer using the Read tool over the Bash tool with commands like cat, head, tail, or sed to read files. This provides better output formatting and filtering
- For open-ended searches that may require multiple rounds of globbing and grepping, use the Agent tool instead

Error handling:
- File not found: returns an error message indicating the path does not exist
- Binary files: detected by extension and returns a message indicating the file cannot be displayed as text
- Files exceeding 32 MB: returns an error; offset/limit cannot bypass the file-size limit, so use Grep to locate content or another suitable file-processing tool
- Offset exceeds file length: returns the actual line count and valid offset range. Do not guess another offset; omit it to restart from the beginning
- Empty files: return an explicit `[EMPTY FILE]` marker instead of a numbered blank line
- Directories: explicitly reports that Read converted the directory to a listing and directs callers to `folder_operations` with `operation="list"`