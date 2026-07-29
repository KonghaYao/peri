Fetches a web page by URL and returns its content as text.

Usage:
- Only http:// and https:// URLs are allowed
- Content is returned as clean text extracted from the page
- Results are truncated at 2000 lines; full content saved to a temp file when truncated
- An optional 'prompt' parameter provides guidance for how to use the fetched content

Security:
- Maximum response size: 10MB
- Request timeout: 30 seconds

URL rules (violations will cause the call to fail):
- Only use URLs from WebSearch results or explicitly provided by the user in their messages. Never fabricate URLs from memory — guessed paths almost always result in 404.
- For raw.githubusercontent.com and similar file-hosting domains: file paths must come from search results or in-page links. Do NOT assemble paths by guessing variants (e.g., appending -dark, -white, -light to filenames).
- Do NOT pass file:// URLs — use the Read tool for local files.