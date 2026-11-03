import {
  projectCreateResponseSchema,
  projectListResponseSchema,
  type ProjectSummary,
} from "./contracts";

/** 项目目录不可达或写入失败；`status` 供界面区分冲突与网络失败。 */
export class ProjectDirectoryUnavailableError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
    this.name = "ProjectDirectoryUnavailableError";
  }
}

export interface ProjectDirectoryRepository {
  list(signal?: AbortSignal): Promise<ProjectSummary[]>;
  create(name: string, signal?: AbortSignal): Promise<ProjectSummary>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/**
 * 项目目录：读 `GET /api/projects`，写 `POST /api/projects`。
 *
 * 与会话目录同源（服务端 sandbox 清单）；前端不缓存项目归属，刷新时整体替换。
 */
export function createHttpProjectRepository(
  base = "",
  fetchImpl: typeof fetch = globalThis.fetch,
): ProjectDirectoryRepository {
  const url = (path: string) => new URL(path, base || globalThis.location.origin).href;

  async function readJson(response: Response): Promise<unknown> {
    return await response.json().catch(() => undefined);
  }

  function failure(body: unknown, status: number): ProjectDirectoryUnavailableError {
    const message = isRecord(body) && typeof body.message === "string" && body.message.length > 0
      ? body.message
      : `项目目录不可用（HTTP ${status}）。`;
    return new ProjectDirectoryUnavailableError(message, status);
  }

  return {
    async list(signal?: AbortSignal): Promise<ProjectSummary[]> {
      let response: Response;
      try {
        response = await fetchImpl(url("/api/projects"), {
          headers: { accept: "application/json" },
          signal,
        });
      } catch (error) {
        if (signal?.aborted) throw error;
        throw new ProjectDirectoryUnavailableError(
          error instanceof Error ? error.message : "项目目录请求失败。", 0);
      }
      const body = await readJson(response);
      const parsed = projectListResponseSchema.safeParse(body);
      if (!response.ok || !parsed.success) throw failure(body, response.status);
      return parsed.data.projects;
    },

    async create(name: string, signal?: AbortSignal): Promise<ProjectSummary> {
      let response: Response;
      try {
        response = await fetchImpl(url("/api/projects"), {
          method: "POST",
          headers: { "content-type": "application/json", accept: "application/json" },
          body: JSON.stringify({ name }),
          signal,
        });
      } catch (error) {
        if (signal?.aborted) throw error;
        throw new ProjectDirectoryUnavailableError(
          error instanceof Error ? error.message : "项目创建请求失败。", 0);
      }
      const body = await readJson(response);
      const parsed = projectCreateResponseSchema.safeParse(body);
      if (!response.ok || !parsed.success) throw failure(body, response.status);
      return parsed.data.project;
    },
  };
}
