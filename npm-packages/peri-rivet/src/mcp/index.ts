export {
  createResourceMcpHttpHandler,
  type ResourceMcpHttpHandler,
  type ResourceMcpHttpOptions,
  type ResourceScopeResolver,
} from './http.js';
export {
  RESOURCE_TOOL_NAMES,
  type ResourceGetArgs,
  resourceGetInput,
  resourceGetOutput,
} from './schemas.js';
export {
  type CreateResourceMcpServerOptions,
  createResourceMcpServer,
  SCOPE_AUTH_EXTRA_KEY,
} from './server.js';
