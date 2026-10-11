import type { WasmAcpTransport } from "@peri-code/sdk/wasm";
import type { Chat } from "../../shared/chat";

export type { Chat } from "../../shared/chat";

export interface ChatRepository {
  list(): Promise<Chat[]>;
  get(id: string): Promise<Chat | null>;
  create(title: string): Promise<Chat>;
}

/** 浏览器宿主的一次启动；失败以 SDK 的宿主启动错误携带清理证据。 */
export type StartHost = (options: { signal: AbortSignal }) => Promise<WasmAcpTransport>;

/** 保活入口：浏览器没有请求生命周期，默认 fire-and-forget。 */
export type Defer = (promise: Promise<unknown>) => void;
