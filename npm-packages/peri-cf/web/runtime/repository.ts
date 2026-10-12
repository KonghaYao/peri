import { TursoStorage, type SessionStorage, type SessionSummary } from "@peri-code/sdk/portable";
import { storageRelayUrl } from "../host/config";
import type { BrowserConfig } from "../settings/config";
import { WORKSPACE_CWD } from "./bootstrap";
import type { Chat } from "../../shared/chat";
import type { ChatRepository } from "./types";

function chatFrom(summary: SessionSummary): Chat {
  return { id: summary.id, title: summary.title ?? "New chat", updatedAt: summary.updatedAt };
}

export class TursoChatRepository implements ChatRepository {
  constructor(private readonly storage: Pick<SessionStorage, "getSessions" | "getSession">,
    private readonly createSession: (title: string) => Promise<string>) {}

  async list(): Promise<Chat[]> {
    return (await this.storage.getSessions(WORKSPACE_CWD)).map(chatFrom);
  }

  async get(id: string): Promise<Chat | null> {
    const summary = await this.storage.getSession(id);
    return summary?.cwd === WORKSPACE_CWD ? chatFrom(summary) : null;
  }

  async create(title: string): Promise<Chat> {
    const id = await this.createSession(title);
    const chat = await this.get(id);
    if (!chat) throw new Error("Created ACP session is not visible in Turso");
    return chat;
  }
}

/** 会话清单经同源 relay 读取；存储上游基址与 token 由调用方从页面启动时的配置注入（Worker 不持凭据）。 */
export function relayStorage(origin: string, config: BrowserConfig): TursoStorage {
  return new TursoStorage({ url: storageRelayUrl(origin, config.storage.url), authToken: config.storage.token });
}

export function chatRepository(origin: string, config: BrowserConfig,
  createSession: (title: string) => Promise<string>): ChatRepository {
  const storage = relayStorage(origin, config);
  return new TursoChatRepository({
    getSessions: (cwd) => storage.getSessions(cwd),
    getSession: (id) => storage.getSession(id),
  }, createSession);
}
