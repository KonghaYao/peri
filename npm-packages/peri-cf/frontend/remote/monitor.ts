import { monitorSnapshotSchema, type MonitorSnapshot } from "./contracts";

/** 监控快照不可达：保留明确失败，不返回伪造的零值。 */
export class MonitorUnavailableError extends Error {
  constructor(message: string, readonly status: number) {
    super(message);
    this.name = "MonitorUnavailableError";
  }
}

export interface MonitorRepository {
  read(signal?: AbortSignal): Promise<MonitorSnapshot>;
}

function envelopeMessage(body: unknown): string | undefined {
  if (body === null || typeof body !== "object" || Array.isArray(body)) return undefined;
  const message = (body as { message?: unknown }).message;
  return typeof message === "string" && message.length > 0 ? message : undefined;
}

/**
 * 只读监控：`GET /api/monitor` 一次取回四段快照。
 *
 * 服务端已去敏；这里只做契约校验，不保留任何原始响应，避免凭据进入展示态。
 */
export function createHttpMonitorRepository(
  base = "",
  fetchImpl: typeof fetch = globalThis.fetch,
): MonitorRepository {
  return {
    async read(signal?: AbortSignal): Promise<MonitorSnapshot> {
      let response: Response;
      try {
        response = await fetchImpl(new URL("/api/monitor", base || globalThis.location.origin).href, {
          headers: { accept: "application/json" },
          signal,
        });
      } catch (error) {
        if (signal?.aborted) throw error;
        throw new MonitorUnavailableError(
          error instanceof Error ? error.message : "监控数据请求失败。",
          0,
        );
      }
      const body = await response.json().catch(() => undefined);
      const parsed = monitorSnapshotSchema.safeParse(body);
      if (!response.ok || !parsed.success) {
        throw new MonitorUnavailableError(
          envelopeMessage(body) ?? `监控数据不可用（HTTP ${response.status}）。`,
          response.status,
        );
      }
      return parsed.data;
    },
  };
}
