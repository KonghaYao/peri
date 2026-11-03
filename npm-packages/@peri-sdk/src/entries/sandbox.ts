/**
 * Server-side sandbox entry (Node 22.12+ or Bun; not browser/WASM/Edge).
 *
 * Exposes a resource manager for multiple sandbox instances and handles that
 * expose one ready sandbox as a {@link WorkspaceRef} plus an HTTP MCP
 * descriptor. The sandbox implementation is injected by the host as a
 * {@link SandboxAdapter}; this entry neither imports nor defaults to a concrete
 * adapter. Session binding is a caller concern; this entry never receives or
 * records a session and never owns session instructions.
 */
export * from "../sandbox/index";
