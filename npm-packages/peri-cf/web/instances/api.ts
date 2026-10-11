import { instanceListSchema, type InstanceList } from '../../shared/instances';

export class InstancesApi {
  constructor(private readonly token: string) {}

  async list(signal?: AbortSignal): Promise<InstanceList> {
    const response = await fetch('/api/instances', {
      cache: 'no-store',
      signal,
      headers: { Authorization: `Bearer ${this.token}` },
    });
    if (!response.ok) {
      if (response.status === 401 || response.status === 403)
        throw new Error('身份验证失败，请在设置中检查访问令牌。');
      throw new Error(`实例观测请求失败（HTTP ${response.status}），请稍后重试。`);
    }
    let payload: unknown;
    try { payload = await response.json(); }
    catch (error) { throw new Error('实例观测响应格式不正确。', { cause: error }); }
    const parsed = instanceListSchema.safeParse(payload);
    if (!parsed.success) throw new Error('实例观测响应格式不正确。');
    return parsed.data;
  }
}
