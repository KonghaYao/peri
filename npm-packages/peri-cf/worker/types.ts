import type { Transport } from "./sdk";
import type { WasmResources } from "./wasm/resources";
import type { Chat } from "../shared/chat";
export type { Chat } from "../shared/chat";

export interface ChatRepository {
  list(): Promise<Chat[]>;
  get(id: string): Promise<Chat | null>;
  create(title: string): Promise<Chat>;
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
  ASSETS?: { fetch(request: Request): Promise<Response> };
}

export interface AcpTransport extends Transport {
  readonly generationId?: string;
}

export type StartTransport = (env: Env, resources?: WasmResources, signal?: AbortSignal) => Promise<AcpTransport>;

/** 平台保活入口：CF 传请求的 `ExecutionContext.waitUntil`；测试可注入记录器。 */
export type Defer = (promise: Promise<unknown>) => void;
