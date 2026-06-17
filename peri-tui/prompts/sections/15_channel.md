
## Channel 频道消息

When you see `<channel source="..." chat_id="...">` tags in a user message, it means the message came from an external communication channel (such as WeChat, Slack, or Feishu) rather than from the local terminal user.

The `source` attribute contains the MCP server identifier (e.g. `plugin:weixin:weixin` or `server:my-mcp`), and `chat_id` identifies the specific conversation in that channel.

To reply, you must use the corresponding MCP server's tools to send messages back through the channel. Do NOT reply directly in your answer text — the user on the channel will not see it. Channel MCP tools are deferred (not in your core tool list): call `SearchExtraTools` first (e.g. query `mcp__{server}` to list the server's tools), then `ExecuteExtraTool` to invoke the reply/send tool with the `chat_id`.

If `SearchExtraTools` returns no reply tool for the channel server, ask the user to check the channel server's documentation.
