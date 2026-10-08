import { chatSchema, chatListSchema, chatDetailSchema, type Chat, type ChatDetail } from '../../shared/chat';

export function errorText(error: unknown): string {
  return (error instanceof Error ? error.message : '请求失败，请稍后重试。')
    .replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, '').slice(0, 512);
}

export class ChatApi {
  constructor(private readonly token: string) {}

  private async request(path: string, options: RequestInit = {}): Promise<Response> {
    const response = await fetch(`/api/chats${path}`, {
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

  async detail(chatId: string, signal?: AbortSignal): Promise<ChatDetail> {
    const payload: unknown = await (await this.request(`/${encodeURIComponent(chatId)}`, { signal })).json();
    const result = chatDetailSchema.safeParse(payload);
    if (!result.success) throw new Error('会话历史格式不正确。');
    if (result.data.chat.id !== chatId) throw new Error('服务端返回了不匹配的会话。');
    return result.data;
  }

  async cancel(chatId: string): Promise<void> {
    await this.request(`/${encodeURIComponent(chatId)}/cancel`, { method: 'POST' });
  }

  async send(chatId: string, content: string, signal?: AbortSignal): Promise<void> {
    const response = await this.request(`/${encodeURIComponent(chatId)}/messages`, {
      method: 'POST', body: JSON.stringify({ content }), signal,
    });
    if (response.status !== 202 || !response.headers.get('content-type')?.includes('application/json')) {
      throw new Error('服务端未确认接受消息。');
    }
    const payload: unknown = await response.json();
    if (!payload || typeof payload !== 'object' || !('accepted' in payload) || payload.accepted !== true) {
      throw new Error('消息接受响应格式不正确。');
    }
  }
}
