Reads a file from the local filesystem. You can access any file directly by using this tool.
Assume this tool is able to read all files on the machine. If the User provides a path to a file assume that path is valid. It is okay to read a file that does not exist; an error will be returned.

Usage:
- The file_path parameter must be an absolute path, not a relative path
- By default, it reads up to 2000 lines starting from the beginning of the file
- You can optionally specify a line offset and limit (especially handy for long files), but it's recommended to read the whole file by not providing these parameters
- Any lines longer than 65536 characters will be truncated
- Results are returned using cat -n format, with line numbers starting at 1
- This tool reads files from the local filesystem; it cannot handle URLs
- You can call multiple tools in a single response. It is always better to speculatively read multiple files before making edits
- You should prefer using the Read tool over the Bash tool with commands like cat, head, tail, or sed to read files. This provides better output formatting and filtering
- For open-ended searches that may require multiple rounds of globbing and grepping, use the Agent tool instead

Error handling:
- File not found: returns an error message indicating the path does not exist
- Binary files: detected by extension and returns a message indicating the file cannot be displayed as text
- Files exceeding 32 MB: returns an error suggesting use of offset/limit parameters
- Offset exceeds file length: returns an error indicating the line range is invalid
- Directories: detected and returns a listing of directory contents with a hint to use folder_operations for advanced folder operations