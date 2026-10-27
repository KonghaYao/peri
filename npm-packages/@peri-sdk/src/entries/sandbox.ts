/**
 * Server-side sandbox entry (Node 22.12+ or Bun; not browser/WASM/Edge).
 *
 * Exposes a resource manager for multiple in-process `peri-sandbox` instances
 * and handles that expose one ready sandbox as `workspacePath` plus an HTTP
 * MCP descriptor. Session binding is a caller concern; this entry never
 * receives or records a session and never owns session instructions.
 */
export * from "../sandbox/index";
