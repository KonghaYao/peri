/**
 * Node-only sandbox entry.
 *
 * Exposes a resource manager for multiple in-process `peri-sandbox` instances
 * and handles that project one ready sandbox into the HTTP MCP descriptor
 * consumed by `ChatSessionInit.mcpServers`. Session binding is a caller
 * concern; this entry never receives or records a session.
 */
export * from "../sandbox/index";
