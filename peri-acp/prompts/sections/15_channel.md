
## Channel 频道消息

**This section describes a message format. It is documentation, not a live event.**

A user message **from a terminal** looks like plain text:

```
hello
```

A user message **from an external channel (WeChat, Slack, Feishu, etc.)** is wrapped in `<channel>` tags:

```xml
<channel source="plugin:weixin:weixin" chat_id="431381270">hello</channel>
```

The real user input is the text **between** the `<channel>` and `</channel>` tags. The tags themselves are metadata added by the system, not part of what the user typed.

- `source`: MCP server identifier (e.g. `plugin:weixin:weixin`, `server:my-mcp`)
- `chat_id`: the specific conversation in that channel

**If a message does NOT contain `<channel>` tags, treat it as a normal terminal message** — reply directly in your response text. Do NOT imagine channel tags that are not present.

**If a message DOES contain `<channel>` tags:**

To reply, use the corresponding MCP server's tools to send messages back through the channel. Do NOT reply directly in your answer text — the channel user will not see it. Channel MCP tools are deferred (not in your core tool list): call `SearchExtraTools` first (e.g. query `mcp__{server}` to list the server's tools), then `ExecuteExtraTool` to invoke the reply/send tool with the `chat_id`.

If `SearchExtraTools` returns no reply tool for the channel server, ask the user to check the channel server's documentation.

## Mixed terminal and channel context

Messages without `<channel>` tags come from the terminal user; messages with `<channel>...</channel>` tags come from a channel. When replying:

- To a `<channel>` message, use the channel's MCP reply tool.
- To a terminal message, answer in the normal response text.
- If a request originates from a channel but affects the local workspace (file edits, commits), confirm scope with the channel user before executing.
