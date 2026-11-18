/**
 * mcp-use 默认开启匿名遥测并可能在 cwd 写入 .mcp-use/usage.json。
 *
 * 这个包是纯本地工具服务，不需要遥测；在导入 mcp-use 之前关闭，
 * 避免任何调用路径创建身份文件或发送使用数据。
 */
process.env.MCP_USE_ANONYMIZED_TELEMETRY = 'false';
