import { Hono, type Context } from "hono";
import {
  parseChatUserAction,
  type ChatBindingTarget,
  type ChatManagerContract,
  type ChatServiceContract,
  type ChatSessionDirectoryEntry,
  type ChatUserAction as ChatUserActionValue,
  type ReadPayloadOptions,
  type ToolPayloadResponse,
} from "@peri-code/sdk";
import { ChatCloseIncompleteError, WorkspaceMismatchError } from "@peri-code/sdk";
import { logError } from "../logging";
import type { MonitorSource } from "../monitor";
import {
  ProjectNameConflictError,
  ProjectNameError,
  ProjectNotFoundError,
  type ProjectView,
} from "../projects";
import { SandboxStartError } from "../sandbox";
import {
  chatNotFound,
  closeIncomplete,
  mapChatActionError,
  mapPayloadError,
} from "./errors";

/** HTTP 适配所需的注册表能力；不依赖父层具体装配类。 */
type ApiRegistry = {
  chats: Pick<ChatManagerContract, "getChat" | "findChatBySession">;
  /** 注入 workspace、agent 与 sandbox 后创建句柄；HTTP 层不构造模型/工具面设置。 */
  createChat(target?: {
    path?: string;
    sessionId?: string;
    projectId?: string;
  }): Promise<ChatServiceContract>;
  /** 合并旧 `/workspace` 与每个 sandbox 工作区的会话目录。 */
  listSessions(path?: string): Promise<(ChatSessionDirectoryEntry & { projectId: string })[]>;
  listProjects(): Promise<ProjectView[]>;
  createProject(name: unknown): Promise<ProjectView>;
  closeChat(id: string): Promise<void>;
  hasPendingClose(id: string): boolean;
};

export interface ApiOptions {
  registry: ApiRegistry;
  monitor: MonitorSource;
}

interface ActionTarget {
  sessionId: string | null;
  bindingRevision: number;
}

const MAX_ACTION_BODY_BYTES = 256 * 1024;
const INVALID_ACTION = "Action body is not a valid ChatUserAction envelope";

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function invalidAction(c: Context, message: string, requestId?: string) {
  return c.json({
    ok: false as const,
    code: "invalid_action",
    message,
    disposition: "rejected" as const,
    ...(requestId ? { requestId } : {}),
  }, 400);
}

/** `target` 是连接所属绑定；空句柄用 `sessionId:null` + `bindingRevision:0`。 */
function parseTarget(value: unknown): ActionTarget | string {
  if (!isRecord(value)) return "target must be an object";
  const sessionId = value.sessionId;
  if (sessionId !== null && (typeof sessionId !== "string" || sessionId.length === 0)) {
    return "target.sessionId must be a non-empty string or null";
  }
  const revision = value.bindingRevision;
  if (typeof revision !== "number" || !Number.isSafeInteger(revision) || revision < 0) {
    return "target.bindingRevision must be a non-negative safe integer";
  }
  return { sessionId, bindingRevision: revision };
}

async function readActionBody(
  c: Context,
): Promise<{ ok: true; value: unknown } | { ok: false; response: Response }> {
  const length = Number(c.req.header("content-length") ?? "0");
  if (Number.isFinite(length) && length > MAX_ACTION_BODY_BYTES) {
    return { ok: false, response: invalidAction(c, "Action body exceeds 256 KiB") };
  }
  const text = await c.req.text();
  if (new TextEncoder().encode(text).byteLength > MAX_ACTION_BODY_BYTES) {
    return { ok: false, response: invalidAction(c, "Action body exceeds 256 KiB") };
  }
  try {
    return { ok: true, value: JSON.parse(text) };
  } catch {
    return { ok: false, response: invalidAction(c, INVALID_ACTION) };
  }
}

interface ExecutedAction {
  target: ChatBindingTarget;
  details: Record<string, unknown>;
}

/** 只在这里分支执行；send 只等待入队接受，不等待整轮投递。 */
async function executeAction(
  chat: ChatServiceContract,
  action: ChatUserActionValue,
  fallback: ActionTarget,
): Promise<ExecutedAction> {
  switch (action.type) {
    case "send": {
      const receipt = chat.execute(action);
      await receipt.accepted;
      return {
        target: receipt.target,
        details: { inputId: receipt.inputId, accepted: true },
      };
    }
    case "create_new_session":
    case "change_session": {
      const result = await chat.execute(action);
      return {
        target: {
          chatId: result.chatId,
          sessionId: result.sessionId,
          bindingRevision: result.bindingRevision,
        },
        details: {},
      };
    }
    case "cancel": {
      const result = await chat.execute(action);
      return {
        target: bindingTarget(chat, fallback),
        details: { status: result.status },
      };
    }
    case "force_send":
    case "take_back":
    case "answer": {
      const result = await chat.execute(action);
      return {
        target: bindingTarget(chat, fallback),
        details: { status: result.status },
      };
    }
    case "set_config_option": {
      const result = await chat.execute(action);
      return {
        target: bindingTarget(chat, fallback),
        details: {
          status: result.status,
          configId: action.payload.configId,
          value: action.payload.value,
        },
      };
    }
    case "load_earlier_history": {
      const result = await chat.execute(action);
      return {
        target: bindingTarget(chat, fallback),
        details: { applied: result.applied, nextCursor: result.nextCursor },
      };
    }
  }
}

function bindingTarget(chat: ChatServiceContract, fallback: ActionTarget): ChatBindingTarget {
  const binding = chat.binding;
  if (binding) return binding;
  return {
    chatId: chat.id,
    sessionId: fallback.sessionId ?? "",
    bindingRevision: fallback.bindingRevision,
  };
}

/**
 * 服务端 HTTP 面：目录、句柄、Action 与工具载荷。
 *
 * 路由只做边界校验、目标绑定核对与错误映射；执行、绑定与关闭语义都在 SDK。
 */
export function createApi(deps: ApiOptions): Hono {
  const app = new Hono();

  // 单个只读快照承载监控面板四段；构建失败整体 503，不返回伪造数据。
  app.get("/api/monitor", async (c) => {
    try {
      return c.json({ ok: true as const, ...await deps.monitor.snapshot() });
    } catch (error) {
      logError("[peri-cf] Server monitor snapshot is unavailable", error);
      return c.json({
        ok: false as const,
        code: "monitor_unavailable",
        message: "Monitor snapshot is unavailable",
      }, 503);
    }
  });

  app.get("/api/chats", async (c) => {
    const raw = c.req.query("path");
    const path = raw && raw.length > 0 ? raw : undefined;
    try {
      const chats = await deps.registry.listSessions(path);
      return c.json({ chats, available: true });
    } catch (error) {
      logError("[peri-cf] Peri session directory is unavailable", error);
      return c.json({
        chats: [],
        available: false,
        error: "Session directory is unavailable",
      }, 503);
    }
  });

  app.get("/api/projects", async (c) => {
    try {
      return c.json({ projects: await deps.registry.listProjects() });
    } catch (error) {
      logError("[peri-cf] Peri project directory is unavailable", error);
      return c.json({
        projects: [],
        available: false,
        code: "projects_unavailable",
        message: "Project directory is unavailable",
      }, 503);
    }
  });

  app.post("/api/projects", async (c) => {
    const body = await c.req.text();
    let parsed: unknown;
    try {
      parsed = JSON.parse(body);
    } catch {
      return c.json({ ok: false, code: "invalid_request", message: "Body is not valid JSON" }, 400);
    }
    if (!isRecord(parsed)) {
      return c.json({ ok: false, code: "invalid_request", message: "Body must be an object" }, 400);
    }
    try {
      const project = await deps.registry.createProject(parsed.name);
      return c.json({ project }, 201);
    } catch (error) {
      if (error instanceof ProjectNameError) {
        return c.json({ ok: false, code: "invalid_project_name", message: error.message }, 400);
      }
      if (error instanceof ProjectNameConflictError) {
        return c.json({ ok: false, code: "project_name_conflict", message: error.message }, 409);
      }
      logError("[peri-cf] Peri project creation failed", error);
      return c.json({
        ok: false,
        code: "projects_unavailable",
        message: "Project directory is unavailable",
      }, 503);
    }
  });

  app.post("/api/chats", async (c) => {
    const body = await c.req.text();
    let parsed: unknown = {};
    if (body.trim().length > 0) {
      try {
        parsed = JSON.parse(body);
      } catch {
        return c.json({ ok: false, code: "invalid_request", message: "Body is not valid JSON" }, 400);
      }
    }
    if (!isRecord(parsed)) {
      return c.json({ ok: false, code: "invalid_request", message: "Body must be an object" }, 400);
    }
    const path = parsed.path;
    if (path !== undefined && (typeof path !== "string" || path.length === 0)) {
      return c.json({ ok: false, code: "invalid_request", message: "path must be a non-empty string" }, 400);
    }
    const sessionId = parsed.sessionId;
    if (sessionId !== undefined && (typeof sessionId !== "string" || sessionId.length === 0)) {
      return c.json({ ok: false, code: "invalid_request", message: "sessionId must be a non-empty string" }, 400);
    }
    if (path !== undefined && sessionId !== undefined) {
      return c.json({ ok: false, code: "invalid_request", message: "path and sessionId are mutually exclusive" }, 400);
    }
    const projectId = parsed.projectId;
    if (projectId !== undefined && (typeof projectId !== "string" || projectId.length === 0)) {
      return c.json({ ok: false, code: "invalid_request", message: "projectId must be a non-empty string" }, 400);
    }
    if (projectId !== undefined && (path !== undefined || sessionId !== undefined)) {
      return c.json({
        ok: false,
        code: "invalid_request",
        message: "projectId cannot be combined with path or sessionId",
      }, 400);
    }
    // 只登记句柄；历史会话的 sandbox 在此按 sessionId 解析并启动。
    let chat: ChatServiceContract;
    try {
      chat = await deps.registry.createChat({ path, sessionId, projectId });
    } catch (error) {
      if (error instanceof WorkspaceMismatchError) {
        return c.json({
          ok: false as const,
          code: "workspace_mismatch",
          message: error.message,
        }, 400);
      }
      if (error instanceof ProjectNotFoundError) {
        return c.json({
          ok: false as const,
          code: "project_not_found",
          message: error.message,
        }, 404);
      }
      if (error instanceof SandboxStartError) {
        logError("[peri-cf] Peri sandbox failed to start", error, { sessionId, projectId });
        return c.json({
          ok: false as const,
          code: "sandbox_start_failed",
          message: "Sandbox failed to start for this session",
        }, 503);
      }
      throw error;
    }
    return c.json({
      chat: {
        id: chat.id,
        sessionId: chat.sessionId,
        bindingRevision: chat.bindingRevision,
      },
    }, 201);
  });

  app.post("/api/chats/:chatId/actions", async (c) => {
    const chatId = c.req.param("chatId");
    const chat = deps.registry.chats.getChat(chatId)
      ?? deps.registry.chats.findChatBySession(chatId);
    if (!chat) return c.json(chatNotFound(chatId).body, 410);

    const body = await readActionBody(c);
    if (!body.ok) return body.response;
    if (!isRecord(body.value)) return invalidAction(c, INVALID_ACTION);

    const target = parseTarget(body.value.target);
    if (typeof target === "string") return invalidAction(c, target);

    let action: ChatUserActionValue;
    try {
      action = parseChatUserAction(body.value);
    } catch (error) {
      const mapped = mapChatActionError(error, { chatId: chat.id });
      return c.json(mapped.body, mapped.status);
    }

    const binding = chat.binding;
    const bindingMatches = binding
      ? binding.sessionId === target.sessionId
        && binding.bindingRevision === target.bindingRevision
      : target.sessionId === null && target.bindingRevision === 0;
    if (!bindingMatches) {
      return c.json({
        ok: false,
        code: "binding_conflict",
        message: "Action target does not match the current binding",
        disposition: "rejected",
        requestId: action.requestId,
        chatId: chat.id,
      }, 409);
    }

    try {
      const executed = await executeAction(chat, action, target);
      return c.json({
        ok: true,
        requestId: action.requestId,
        chatId: executed.target.chatId,
        sessionId: executed.target.sessionId,
        bindingRevision: executed.target.bindingRevision,
        ...executed.details,
      }, 202);
    } catch (error) {
      const mapped = mapChatActionError(error, {
        chatId: chat.id,
        requestId: action.requestId,
      });
      if (mapped.status >= 500) {
        logError("[peri-cf] Peri chat action failed", error, {
          chatId: chat.id,
          op: action.type,
          requestId: action.requestId,
        });
      }
      return c.json(mapped.body, mapped.status);
    }
  });

  app.delete("/api/chats/:chatId", async (c) => {
    const requestedId = c.req.param("chatId");
    const chat = deps.registry.chats.getChat(requestedId)
      ?? deps.registry.chats.findChatBySession(requestedId);
    const chatId = chat?.id ?? requestedId;
    // 首次关闭可能已移除句柄、只留下待释放的 View；此时同一 DELETE 必须继续重试，
    // 不能把“部分关闭”误报为未知 Chat。
    if (!chat && !deps.registry.hasPendingClose(chatId)) {
      return c.json(chatNotFound(requestedId).body, 410);
    }
    try {
      await deps.registry.closeChat(chatId);
      return c.body(null, 204);
    } catch (error) {
      if (error instanceof ChatCloseIncompleteError) {
        logError("[peri-cf] Peri chat close is incomplete", error, { chatId });
        const mapped = closeIncomplete(error, chatId);
        return c.json(mapped.body, 500);
      }
      const mapped = mapChatActionError(error, { chatId });
      logError("[peri-cf] Peri chat close failed", error, { chatId });
      return c.json(mapped.body, mapped.status);
    }
  });

  app.get("/api/chats/:chatId/payload/:refId", async (c) => {
    const chatId = c.req.param("chatId");
    const chat = deps.registry.chats.getChat(chatId)
      ?? deps.registry.chats.findChatBySession(chatId);
    if (!chat) return c.json(chatNotFound(chatId).body, 410);

    let refId: string;
    try {
      refId = decodeURIComponent(c.req.param("refId"));
    } catch {
      return c.json({ ok: false, code: "invalid_payload_ref", message: "Invalid payload id" }, 400);
    }
    const rawBytes = c.req.query("bytes");
    if (!rawBytes || !/^\d+$/.test(rawBytes)) {
      return c.json({
        ok: false,
        code: "invalid_payload_ref",
        message: "bytes must be a non-negative integer",
      }, 400);
    }
    const bytes = Number(rawBytes);
    if (!Number.isSafeInteger(bytes) || bytes < 0) {
      return c.json({
        ok: false,
        code: "invalid_payload_ref",
        message: "bytes must be a non-negative safe integer",
      }, 400);
    }

    try {
      const options: ReadPayloadOptions = { signal: c.req.raw.signal };
      const payload: ToolPayloadResponse = await chat.readPayload({ id: refId, bytes }, options);
      return c.json({ ok: true, payload });
    } catch (error) {
      const mapped = mapPayloadError(error);
      if (mapped.status >= 500) {
        logError("[peri-cf] Peri payload read failed", error, { chatId: chat.id });
      }
      return c.json(mapped.body, mapped.status);
    }
  });

  app.onError((error, c) => {
    logError("[peri-cf] Peri server request failed", error);
    return c.json({ ok: false, code: "internal_error", message: "Internal server error" }, 500);
  });

  return app;
}

export {
  MAX_CLIENT_SYNC_FRAME_BYTES,
  SYNC_PATH,
  syncEvents,
  syncUpgradeGuard,
  WebSocketSyncChannel,
  type SyncConnection,
  type SyncEnv,
} from "./sync";
