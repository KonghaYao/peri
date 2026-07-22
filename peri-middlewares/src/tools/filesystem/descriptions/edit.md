Performs exact string replacements in files.

Usage:
- You must use your Read tool at least once in the conversation before editing. This tool will fail if you attempt an edit without reading the file
- When editing text from Read tool output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix
- ALWAYS prefer editing existing files in the codebase. DO NOT create new files unless explicitly required
- The file_path parameter must be an absolute path, not a relative path
- The old_string parameter must match exactly, including all whitespace and indentation
- The edit will FAIL if old_string is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use replace_all to change every instance of old_string
- Use replace_all for replacing and renaming strings across the file

Error handling:
- old_string not found: returns an error indicating the string does not exist in the file
- old_string not unique: returns an error with the count of occurrences, suggesting more context or replace_all
- old_string is empty: returns an error rejecting the operation
- File not found: returns an error indicating the path does not exist