/**
 * 访问令牌的浏览器持久化边界：localStorage 是唯一存储位置，读写与跨标签同步都收敛在这里。
 * 令牌只经 Authorization 头与 WS 认证帧发送，不进入 URL、日志与构建产物。
 */
export const tokenStorageKey = 'peri.access-token';

/** 令牌会进入 Authorization 头，空白与控制字符会破坏请求头或造成注入。 */
const invalidTokenCharacters = /[\u0000-\u0020\u007f]/;

export type TokenSaveFailure = 'invalid-token' | 'storage-unavailable';

/* 禁用存储、隐私模式或沙箱 iframe 下访问 localStorage 本身可能抛错。 */
function storage(): Storage | null {
  try { return window.localStorage; }
  catch { return null; }
}

/** 读取持久化令牌：缺失、不可读或格式非法一律按未设置处理。 */
export function readToken(): string {
  try {
    const stored = storage()?.getItem(tokenStorageKey) ?? '';
    return stored && !invalidTokenCharacters.test(stored) ? stored : '';
  } catch { return ''; }
}

/** 写入或清除令牌，返回失败原因；成功返回 null。 */
export function saveToken(token: string): TokenSaveFailure | null {
  if (invalidTokenCharacters.test(token)) return 'invalid-token';
  const area = storage();
  if (!area) return 'storage-unavailable';
  try {
    if (token) area.setItem(tokenStorageKey, token);
    else area.removeItem(tokenStorageKey);
    return null;
  } catch { return 'storage-unavailable'; }
}

/** 订阅其他标签页的令牌变化；写入方标签页不会收到自己的事件，key 为 null 表示整区被清空。 */
export function subscribeToken(listener: (token: string) => void): () => void {
  const handle = (event: StorageEvent) => {
    if (event.storageArea !== storage()) return;
    if (event.key !== null && event.key !== tokenStorageKey) return;
    listener(readToken());
  };
  window.addEventListener('storage', handle);
  return () => window.removeEventListener('storage', handle);
}
