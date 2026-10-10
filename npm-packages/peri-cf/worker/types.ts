import type { Transport } from "./sdk";
import type { WasmResources } from "./wasm/resources";
import type { Chat, PersistedMessage as Message } from "../shared/chat";
export type { Chat, PersistedMessage as Message } from "../shared/chat";

export interface ChatRepository {
  list(): Promise<Chat[]>;
  get(id: string): Promise<Chat | null>;
  create(title: string): Promise<Chat>;
}

export interface SessionRecord {
  chat: Chat;
  messages: Message[];
  running?: boolean;
  executionBlocked?: boolean;
}

export interface ChatNamespace {
  idFromName(name: string): unknown;
  get(id: any): { fetch(request: Request): Promise<Response> };
}

export interface Env {
  APP_AUTH_TOKEN?: string;
  PERI_STORAGE_URL?: string;
  PERI_STORAGE_TOKEN?: string;
  PERI_MACHINE_ID?: string;
  MODEL_BASE_URL?: string;
  MODEL_API_KEY?: string;
  MODEL_ID?: string;
  MODEL_PROVIDER?: string;
  PERI_DNS_OVERRIDES?: string;
  CHAT_SESSIONS: ChatNamespace;
  ASSETS?: { fetch(request: Request): Promise<Response> };
}

/** DO 存储端口的应用侧最小契约（与 DurableObjectStorage 的成组 put / 数组 delete / prefix list 对齐）。 */
export interface SessionState {
  storage: {
    get<T>(key: string): Promise<T | undefined>;
    put(entries: Record<string, unknown>): Promise<void>;
    delete(keys: string[]): Promise<boolean | void>;
    list(options?: { prefix?: string; limit?: number }): Promise<Map<string, unknown>>;
  };
  waitUntil(promise: Promise<unknown>): void;
}

export interface AcpTransport extends Transport {
  readonly generationId?: string;
}

export type StartTransport = (env: Env, resources?: WasmResources, signal?: AbortSignal) => Promise<AcpTransport>;
