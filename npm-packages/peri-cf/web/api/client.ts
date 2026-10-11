import type { PeriApplication } from '../runtime/application';
import type { Chat } from '../../shared/chat';
import type { SyncState } from '../../shared/sync';

/** 错误文案单一出口：宿主字符串与 Error 用同一条清洗规则，避免控制字符进入视图。 */
export function errorText(error: unknown): string {
  const text = typeof error === 'string' ? error : error instanceof Error ? error.message : '本地宿主调用失败，请稍后重试。';
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, '').slice(0, 512);
}

/** 只读消费面：会话清单/创建/详情都由本地应用门面提供；命令走会话连接（见 runtime/connection）。 */
export class ChatApi {
  constructor(private readonly application: PeriApplication) {}

  async list(signal?: AbortSignal): Promise<Chat[]> {
    signal?.throwIfAborted();
    return await this.application.repository.list();
  }

  async create(signal?: AbortSignal): Promise<Chat> {
    signal?.throwIfAborted();
    return await this.application.repository.create('New chat');
  }

  async detail(chatId: string, signal?: AbortSignal): Promise<SyncState> {
    signal?.throwIfAborted();
    const state = await this.application.runtimeFor(chatId).read();
    if (state.chat.id !== chatId) throw new Error('本地宿主返回了不匹配的会话。');
    if (state.readiness === 'unsupported')
      throw new Error('会话使用了不受支持的数据版本，请刷新页面或升级应用后再继续。');
    return state;
  }
}
