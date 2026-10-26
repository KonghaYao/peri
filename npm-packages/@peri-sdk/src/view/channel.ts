/**
 * ChatView / ChatReplica 使用的逻辑同步通道（Phase 0 冻结）。
 *
 * 只约定帧的收发与关闭，不创建 WebSocket/HTTP，也不依赖 Node 或浏览器 API；
 * 鉴权与物理连接由应用在调用 `sync`/`connect` 前完成。帧内容由上层 Yjs
 * 同步协议解释，SDK 不通过该通道发送凭据或执行答案。
 */
export interface ChatYjsChannel {
    readonly closed: boolean;
    send(frame: Uint8Array): void | Promise<void>;
    onMessage(listener: (frame: Uint8Array) => void): () => void;
    onClose(listener: (error?: Error) => void): () => void;
    close(reason?: Error): void | Promise<void>;
}
