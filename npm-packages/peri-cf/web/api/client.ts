import { chatSchema, chatListSchema, type Chat } from '../../shared/chat';
import { syncStateSchema, type SyncState } from '../../shared/sync';

/** 错误文案单一出口：服务端字符串与 Error 用同一条清洗规则，避免控制字符进入视图。 */
export function errorText(error: unknown): string {
  const text = typeof error === 'string' ? error : error instanceof Error ? error.message : '请求失败，请稍后重试。';
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, '').slice(0, 512);
}

/** 只读消费面：会话命令（发送/取消/交互回答/载荷全文）一律走 WS 控制帧，HTTP 不再承载。 */
export class ChatApi {
  constructor(private readonly token: string) {}

  private async request(path: string, options: RequestInit = {}): Promise<Response> {
    const response = await fetch(`/api/v2/chats${path}`, {
      ...options,
      cache: 'no-store',
      headers: {
        Authorization: `Bearer ${this.token}`,
        ...(options.body ? { 'Content-Type': 'application/json' } : {}),
        ...options.headers,
      },
    });
    if (!response.ok) {
      if (response.status === 401 || response.status === 403) {
        throw new Error('身份验证失败，请在设置中检查访问令牌。');
      }
      // 服务端已把会话命令入口改为 426：旧页面命中时给出的唯一可行动作是刷新。
      if (response.status === 426) {
        throw new Error('客户端版本过旧，请刷新页面后重试。');
      }
      if (response.headers.get('content-type')?.includes('application/json')) {
        let payload: unknown;
        try { payload = await response.json(); }
        catch (error) { throw new Error(`请求失败（HTTP ${response.status}），错误响应格式不正确。`, { cause: error }); }
        if (payload && typeof payload === 'object' && 'error' in payload && typeof payload.error === 'string')
          throw new Error(`HTTP ${response.status}: ${payload.error}`);
      }
      throw new Error(`请求失败（HTTP ${response.status}），请稍后重试。`);
    }
    return response;
  }

  async list(signal?: AbortSignal): Promise<Chat[]> {
    const payload: unknown = await (await this.request('', { signal })).json();
    const result = chatListSchema.safeParse(payload);
    if (!result.success) {
      throw new Error('会话列表格式不正确。');
    }
    return result.data.chats;
  }

  async create(signal?: AbortSignal): Promise<Chat> {
    const payload: unknown = await (await this.request('', {
      method: 'POST', body: JSON.stringify({}), signal,
    })).json();
    const result = chatSchema.safeParse(payload);
    if (!result.success) throw new Error('新建会话响应格式不正确。');
    return result.data;
  }

  async detail(chatId: string, signal?: AbortSignal): Promise<SyncState> {
    const payload: unknown = await (await this.request(`/${encodeURIComponent(chatId)}`, { signal })).json();
    const result = syncStateSchema.safeParse(payload);
    if (!result.success) throw new Error('会话历史格式不正确。');
    if (result.data.chat.id !== chatId) throw new Error('服务端返回了不匹配的会话。');
    if (result.data.readiness === 'unsupported') throw new Error('会话使用了不受支持的数据版本，请刷新页面或升级应用后再继续。');
    return result.data;
  }
}
