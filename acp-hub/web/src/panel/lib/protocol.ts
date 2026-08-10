// acp-hub Web 面板 —— 帧协议模块（移植自原 protocol.js，TS 化）。
//
// 与 server 的 client 面协议逐字对齐（M3 方案 §1）：
//   - payload 字段 camelCase；`t` 为顶层判别字段；
//   - `action_ack.status` 为小写 snake_case（accepted/committed/duplicate）；
//   - `action_error.code` 为 SCREAMING_SNAKE_CASE；
//   - ysync 订阅字段名是 `docs`（DocId 透明字符串）；
//   - 快照帧带 `projectionVersion`，增量帧不带（serde skip，不输出 null）。
//
// 本模块只负责帧的构造/解析与 base64 工具，不持有任何连接状态
// （连接状态机见 ws-client.ts）。

/** 注册表 doc id（与 proto/src/conn.rs 的 DocId::REGISTRY 对齐），常驻订阅。 */
export const DOC_REGISTRY = 'hub:registry';

/** chat 派生 doc id（订阅字段透明字符串，前缀区分投影）。 */
export const chatDoc = (sid: string): string => `chat:${sid}`;
export const controlDoc = (sid: string): string => `control:${sid}`;

/** action 幂等键：crypto.randomUUID（浏览器全局；localhost/https 下可用）。 */
export const newCommandId = (): string => crypto.randomUUID();

// ── 上行帧 ──────────────────────────────────────────────────────────────

/** 认证帧：ws 握手后第一帧必须是它（否则 server 以 1011 关闭）。client 面
 *  无 HMAC（§9.2 仅覆盖 instance 连接），token 为 44 字符 base64，顶层字段。
 *  认证失败 → server 以 4502 关闭。 */
export const auth = (token: string) => ({ t: 'auth', token });

/** ysync 订阅：字段名 `docs`；首个订阅后 server 推各 doc 快照 + ready。 */
export const subscribe = (docs: string[]) => ({ t: 'ysync.subscribe', docs });

/** ysync 退订：幂等，重复退订无副作用。 */
export const unsubscribe = (docs: string[]) => ({ t: 'ysync.unsubscribe', docs });

/** 心跳应答：server 每 5s 发 keep_alive，15s（3×间隔）内不回 → 4501 关闭。 */
export const pong = () => ({ t: 'pong' });

// ── action 帧 ───────────────────────────────────────────────────────────

/** 外层包装：t/commandId/type/payload 四字段；commandId 为幂等键
 *  （重试复用同一 commandId，server 幂等去重 → duplicate ack）。 */
export const action = (type: string, payload: Record<string, unknown>) => ({
  t: 'action',
  commandId: newCommandId(),
  type,
  payload,
});

/** 新建对话：三字段全可选（缺省 = 本机）。committed ack 携带的 chatId
 *  是 server 生成 id 的唯一告知路径 → main 据此自动补订阅并选中。
 *
 *  `acpSessionId`（ACP 历史会话 id，session/list 返回）：携带时 create 走
 *  `session/load`（§8.5 历史恢复）——回放 ACP agent 磁盘会话内容到新对话。
 *
 *  `workspaceId`（归属工作区，§6.3 workspace 扩展）：携带时 cwd 继承自
 *  workspace 定义（server 侧解析，不信任客户端直传 cwd）。 */
export const createChat = (title?: string, acpSessionId?: string, workspaceId?: string) => {
  const payload: Record<string, unknown> = {};
  if (title) payload.title = title;
  if (acpSessionId) payload.acpSessionId = acpSessionId;
  if (workspaceId) payload.workspaceId = workspaceId;
  return action('chat/create', payload);
};

/** 新建工作区：定义本地目录 cwd，其下新建对话继承该目录（§6.3 workspace
 *  扩展）。管理面命令（独立于 chat）：accepted → committed 直通，无队列。 */
export const workspaceCreate = (name: string, cwd: string) =>
  action('workspace/create', { name, cwd });

/** 删除工作区定义（不影响已建对话/会话；仅移除 Registry Doc 条目）。 */
export const workspaceRemove = (workspaceId: string) =>
  action('workspace/remove', { workspaceId });

/** 按需查询指定对话的 ACP 会话列表（§6.3）：server 从 chat record 解析
 *  cwd 向 agent 侧发 session/list RPC，结果经 `session_list` 下行帧回投
 *  （agent 侧是真实数据源，非轮询投影过滤）。 */
export const sessionList = (chatId: string) => action('session/list', { chatId });

/** §8.5 会话切换：在当前对话（其 ACP 进程）内 load 目标历史会话——
 *  不新建对话/进程（会话是进程内实体；点击 SessionList 历史会话即切换）。 */
export const loadChat = (chatId: string, acpSessionId: string) =>
  action('chat/load', { chatId, acpSessionId });

/** 发送消息（两阶段 ack：accepted → committed）。 */
export const prompt = (chatId: string, message: string) =>
  action('chat/prompt', { chatId, message });

/** 取消当前 turn。 */
export const cancel = (chatId: string) => action('chat/cancel', { chatId });

/** 关闭对话。 */
export const close = (chatId: string) => action('chat/close', { chatId });

/** 权限裁决：decision 仅 "allow" | "deny"。 */
export const resolvePermission = (chatId: string, permissionId: string, decision: string) =>
  action('permission/resolve', { chatId, permissionId, decision });

// ── 下行解析 ────────────────────────────────────────────────────────────

/** 解析下行文本帧为对象；非 JSON → null（调用方记日志即可，不抛异常）。 */
export const parse = (text: string): Record<string, unknown> | null => {
  try {
    return JSON.parse(text) as Record<string, unknown>;
  } catch {
    return null;
  }
};

// ── base64 ↔ Uint8Array（浏览器内置 atob/btoa）──────────────────────────

/** base64 → Uint8Array：yrs v1 update 载荷（快照/增量同格式）。 */
export function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) {
    bytes[i] = bin.charCodeAt(i);
  }
  return bytes;
}

/** Uint8Array → base64（当前仅调试输出用）。 */
export function bytesToBase64(bytes: Uint8Array): string {
  let bin = '';
  for (let i = 0; i < bytes.length; i++) {
    bin += String.fromCharCode(bytes[i]);
  }
  return btoa(bin);
}
