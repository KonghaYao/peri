import './telemetry.js';
import { createServer, type Server as NodeHttpServer } from 'node:http';
import { MCPServer } from 'mcp-use';
import { toNodeHandler } from 'mcp-use/node';
import { LOCAL_HOSTNAMES, LOOPBACK_HOST, MCP_PATH, PACKAGE_NAME, PACKAGE_VERSION } from './constants.js';
import { renderInstructionsPage } from './instructions-page.js';
import { registerTools } from './tools.js';

export function isAllowedHostHeader(host: string | undefined): boolean {
  if (host === undefined || host === '') return false;
  // 用户信息、路径和空白不属于 Host 值；拒绝这些伪装形式。
  if (host.includes('@') || host.includes('/') || /[\s,]/.test(host)) return false;
  try {
    const url = new URL(`http://${host}`);
    if (url.username !== '' || url.password !== '' || url.pathname !== '/' || url.search !== '' || url.hash !== '') {
      return false;
    }
    return LOCAL_HOSTNAMES.includes(url.hostname);
  } catch {
    return false;
  }
}

export function isAllowedOrigin(origin: string | undefined): boolean {
  // 非浏览器 MCP 客户端通常不带 Origin；显式空值属于异常来源。
  if (origin === undefined) return true;
  if (origin === '') return false;
  try {
    const url = new URL(origin);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return false;
    if (url.username !== '' || url.password !== '') return false;
    if (url.pathname !== '/' || url.search !== '' || url.hash !== '') return false;
    return LOCAL_HOSTNAMES.includes(url.hostname);
  } catch {
    return false;
  }
}

function acceptsHtml(accept: string | undefined): boolean {
  if (accept === undefined) return false;
  return accept.split(',').some((range) => range.split(';')[0]?.trim().toLowerCase() === 'text/html');
}

/** 只取路径用于根页面判断；解析失败时交给原 MCP 路由处理。 */
function requestPath(url: string | undefined): string | undefined {
  if (url === undefined) return undefined;
  try {
    return new URL(url, `http://${LOOPBACK_HOST}`).pathname;
  } catch {
    return undefined;
  }
}

export function createMcpServer(databasePath: string): MCPServer {
  const server = new MCPServer({
    name: PACKAGE_NAME,
    version: PACKAGE_VERSION,
    description: '本地 SQLite 数据库 MCP 服务器（仅 Streamable HTTP，回环监听）',
    instructions:
      '数据库路径由启动参数固定，工具不能切换数据库。execute_sql 一次只接受一条 SQL；' +
      '结果最多返回 100 行，truncated 只表示响应截断。每次调用使用独立连接，事务不会跨调用保留。',
    basePath: MCP_PATH,
    host: LOOPBACK_HOST,
    // 显式启用 mcp-use 的 Host/Origin 校验；列表只含回环类主机名。
    allowedHosts: [...LOCAL_HOSTNAMES],
    allowedOrigins: [...LOCAL_HOSTNAMES],
    legacy: 'stateless',
    // 关闭自动 skills 发现、请求日志与 CORS；不引入浏览器访问面。
    skills: false,
    logging: { enabled: false },
  });

  // 仅暴露 /mcp；Host/Origin 双重校验，且不注册任何 CORS 或浏览器页面。
  server.use('*', async (context, next) => {
    if (!isAllowedHostHeader(context.req.header('host'))) return context.text('Forbidden', 403);
    if (!isAllowedOrigin(context.req.header('origin'))) return context.text('Forbidden', 403);

    const path = new URL(context.req.url).pathname;
    if (path !== MCP_PATH) return context.text('Not Found', 404);
    if ((context.req.method === 'GET' || context.req.method === 'HEAD') && acceptsHtml(context.req.header('accept'))) {
      return context.text('Not Found', 404);
    }
    await next();
  });
  registerTools(server, databasePath);
  return server;
}

export interface StartedServer {
  port: number;
  url: string;
  close(): Promise<void>;
}

/**
 * 自己持有 Node HTTP 监听器，先校验原始 Host/Origin 再转成 Web Request。
 *
 * mcp-use 的 `listen()` 在把带 userinfo 的 Host 转成 URL 时会抛错并返回 500；
 * 前置校验可以让所有伪造来源得到稳定的 403，同时保持显式 127.0.0.1 绑定。
 */
export async function startServer(options: { databasePath: string; port: number }): Promise<StartedServer> {
  const mcpServer = createMcpServer(options.databasePath);
  const nodeHandler = toNodeHandler(mcpServer);
  let listeningPort = options.port;

  const httpServer: NodeHttpServer = createServer((request, response) => {
    const host = request.headers.host;
    const origin = request.headers.origin;
    if (Array.isArray(host) || Array.isArray(origin) || !isAllowedHostHeader(host) || !isAllowedOrigin(origin)) {
      request.resume();
      response.writeHead(403, { 'content-type': 'text/plain; charset=utf-8', connection: 'close' });
      response.end('Forbidden');
      return;
    }

    // 根页面只提供说明，不参与 MCP 路由，也不读取 Host/Origin 之外的信息。
    if ((request.method === 'GET' || request.method === 'HEAD') && requestPath(request.url) === '/') {
      const html = renderInstructionsPage(listeningPort);
      response.writeHead(200, {
        'content-type': 'text/html; charset=utf-8',
        'content-length': Buffer.byteLength(html),
        'cache-control': 'no-store',
        'x-content-type-options': 'nosniff',
      });
      response.end(request.method === 'HEAD' ? undefined : html);
      return;
    }

    void nodeHandler(request, response).catch(() => {
      if (!response.headersSent) response.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' });
      response.end('Internal Server Error');
    });
  });

  try {
    await new Promise<void>((resolve, reject) => {
      const onError = (error: Error) => {
        httpServer.off('listening', onListening);
        reject(error);
      };
      const onListening = () => {
        httpServer.off('error', onError);
        resolve();
      };
      httpServer.once('error', onError);
      httpServer.once('listening', onListening);
      httpServer.listen(options.port, LOOPBACK_HOST);
    });
  } catch (error) {
    await mcpServer.close().catch(() => undefined);
    throw error;
  }

  const address = httpServer.address();
  const port = typeof address === 'object' && address !== null ? address.port : options.port;
  listeningPort = port;
  let closed = false;

  return {
    port,
    url: `http://${LOOPBACK_HOST}:${port}${MCP_PATH}`,
    async close() {
      if (closed) return;
      closed = true;
      await mcpServer.close().catch(() => undefined);
      await new Promise<void>((resolve) => {
        httpServer.close(() => resolve());
        httpServer.closeAllConnections();
      });
    },
  };
}
