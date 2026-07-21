Fetches a web page by URL and returns its content as text.

Usage:
- Only http:// and https:// URLs are allowed
- Content is returned as clean text extracted from the page
- Results are truncated at 2000 lines; full content saved to a temp file when truncated
- An optional 'prompt' parameter provides guidance for how to use the fetched content

Security:
- Maximum response size: 10MB
- Request timeout: 30 seconds