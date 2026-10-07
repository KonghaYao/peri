import { TursoStorage, type SessionStorage, type SessionSummary } from "../sdk";
import { WORKSPACE_CWD } from "./bootstrap";
import type { Chat, ChatRepository, Env } from "../types";

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

export function chatRepository(env: Env, createSession: (title: string) => Promise<string>): ChatRepository {
  function storage(): TursoStorage {
    if (!env.PERI_STORAGE_URL || !env.PERI_STORAGE_TOKEN) throw new Error("Turso storage is not configured");
    return new TursoStorage({ url: env.PERI_STORAGE_URL, authToken: env.PERI_STORAGE_TOKEN });
  }
  return new TursoChatRepository({
    getSessions: (cwd) => storage().getSessions(cwd),
    getSession: (id) => storage().getSession(id),
  }, createSession);
}
