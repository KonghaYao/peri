/**
 * 零配置 Worker 的部署环境：只有静态资源绑定，不持有上游地址、凭据或会话状态。
 * 上游目标与凭据都由请求携带（浏览器 BYOK），Worker 纯透传。
 */
export interface Env {
  ASSETS?: { fetch(request: Request): Promise<Response> };
}
