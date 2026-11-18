/** 包与协议边界共用的常量。 */

export const PACKAGE_NAME = 'sqlite-mcp';
export const PACKAGE_VERSION = '0.1.0';

export const DEFAULT_PORT = 31339;
export const LOOPBACK_HOST = '127.0.0.1';
export const MCP_PATH = '/mcp';

/** 默认只截断响应，不向 SQL 注入 LIMIT。 */
export const MAX_RESULT_ROWS = 100;

/** Host/Origin 只接受回环类主机名；端口由实际监听地址决定，不在这里枚举。 */
export const LOCAL_HOSTNAMES: readonly string[] = ['localhost', '127.0.0.1', '[::1]'];
