Fetches a web page by URL and returns its content as text.

Usage:
- Only http:// and https:// URLs are allowed; other schemes are rejected locally before any request is sent
- Content is returned as clean text extracted from the page
- Results are truncated at 2000 lines; full content saved to a temp file when truncated
- An optional 'prompt' parameter provides guidance for how to use the fetched content

Security:
- Maximum extract API response size: 10 MiB (enforced locally). Source page fetch and size limits are enforced by the extraction service.
- Request timeout: 30 seconds

URL rules (violations will cause the call to fail):
- Only use URLs from WebSearch results or explicitly provided by the user in their messages. Never fabricate URLs from memory — guessed paths almost always result in 404.
- For raw.githubusercontent.com and similar file-hosting domains: file paths must come from search results or in-page links. Do NOT assemble paths by guessing variants (e.g., appending -dark, -white, -light to filenames).
- Do NOT pass file:// URLs — use the Read tool for local files.

Boundary: this client validates the URL scheme and bounds the extract API response. Source-page access policy (redirects, private networks, content type, robots) is owned by the extraction service.
