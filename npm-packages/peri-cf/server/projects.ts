import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, relative } from "node:path";
import { logError } from "./logging";

/** 默认项目不是持久化记录：它聚合没有项目归属的历史会话。 */
export const DEFAULT_PROJECT_ID = "default";
export const DEFAULT_PROJECT_NAME = "默认项目";

const PROJECT_FILE_VERSION = 1;
const PROJECT_NAME_MAX_LENGTH = 64;
const WORKSPACE_DIRECTORY = "workspace";

export interface ProjectRecord {
  id: string;
  name: string;
  /** 首次在项目内创建会话时绑定；为空表示项目还没有工作区。 */
  sandboxId: string | null;
  createdAt: string;
}

export interface ProjectView {
  id: string;
  name: string;
  createdAt: string;
  isDefault: boolean;
}

/** 名称不合法；HTTP 层映射为 400。 */
export class ProjectNameError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProjectNameError";
  }
}

/** 名称重复；HTTP 层映射为 409。 */
export class ProjectNameConflictError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProjectNameConflictError";
  }
}

/** 目标项目不存在；HTTP 层映射为 404。 */
export class ProjectNotFoundError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProjectNotFoundError";
  }
}

interface ProjectFile {
  version: typeof PROJECT_FILE_VERSION;
  projects: ProjectRecord[];
}

export interface ProjectStoreOptions {
  /** 测试注入；缺省为 `<sandboxesRoot>/projects.json`。 */
  filePath?: string;
  now?: () => Date;
  idFactory?: () => string;
}

/**
 * 项目清单：与 sandbox 工作区同源的单个 JSON 文件。
 *
 * 项目不进入 Store 数据库：它只描述“哪些 sandbox 工作区属于同一个项目”，
 * 会话归属由 cwd 推导。清单写入串行化并原子替换，避免半写文件。
 */
export class ProjectStore {
  private readonly filePath: string;
  private readonly now: () => Date;
  private readonly idFactory: () => string;
  private loaded: ProjectRecord[] | null = null;
  private loadInFlight: Promise<ProjectRecord[]> | null = null;
  private writeChain: Promise<void> = Promise.resolve();

  constructor(sandboxesRoot: string, options: ProjectStoreOptions = {}) {
    this.filePath = options.filePath ?? join(sandboxesRoot, "projects.json");
    this.now = options.now ?? (() => new Date());
    this.idFactory = options.idFactory ?? (() => `prj_${randomUUID()}`);
  }

  async list(): Promise<ProjectRecord[]> {
    return (await this.load()).map((project) => ({ ...project }));
  }

  /** 读面：默认项目始终存在，其余按清单顺序。 */
  async listViews(): Promise<ProjectView[]> {
    const records = await this.load();
    return [defaultProjectView(), ...records.map(toView)];
  }

  async get(id: string): Promise<ProjectRecord | undefined> {
    const project = (await this.load()).find((item) => item.id === id);
    return project ? { ...project } : undefined;
  }

  async create(rawName: unknown): Promise<ProjectView> {
    const name = normalizeProjectName(rawName);
    const created = await this.mutate(async (projects) => {
      if (name.toLocaleLowerCase() === DEFAULT_PROJECT_NAME.toLocaleLowerCase()
        || projects.some((project) => project.name.toLocaleLowerCase() === name.toLocaleLowerCase())) {
        throw new ProjectNameConflictError(`项目名称已存在：${name}`);
      }
      const project: ProjectRecord = {
        id: this.idFactory(),
        name,
        sandboxId: null,
        createdAt: this.now().toISOString(),
      };
      return { next: [...projects, project], result: project };
    });
    return toView(created);
  }

  /**
   * 首次创建会话时记录项目工作区。
   *
   * 已绑定时保留原 sandboxId：并发创建要复用同一个工作区，不能覆盖。
   */
  async assignSandbox(id: string, sandboxId: string): Promise<ProjectRecord> {
    return await this.mutate(async (projects) => {
      const index = projects.findIndex((project) => project.id === id);
      if (index < 0) throw new ProjectNotFoundError(`项目不存在：${id}`);
      const current = projects[index]!;
      if (current.sandboxId !== null) return { next: projects, result: current };
      const updated: ProjectRecord = { ...current, sandboxId };
      const next = projects.map((project, at) => (at === index ? updated : project));
      return { next, result: updated };
    });
  }

  /**
   * 会话 cwd → 项目归属；未命中任何项目工作区时归入默认项目。
   *
   * 只做目录前缀判断，不查询 Store；同一项目内后续允许更深的 sessionPath。
   */
  projectIdForCwd(cwd: string, projects: readonly ProjectRecord[], sandboxesRoot: string): string {
    for (const project of projects) {
      if (project.sandboxId === null) continue;
      const workspace = join(sandboxesRoot, project.sandboxId, WORKSPACE_DIRECTORY);
      const offset = relative(workspace, cwd);
      if (offset === "" || (!offset.startsWith("..") && !isAbsolute(offset))) return project.id;
    }
    return DEFAULT_PROJECT_ID;
  }

  private async load(): Promise<ProjectRecord[]> {
    if (this.loaded !== null) return this.loaded;
    if (this.loadInFlight !== null) return await this.loadInFlight;
    this.loadInFlight = this.readManifest();
    try {
      return await this.loadInFlight;
    } finally {
      this.loadInFlight = null;
    }
  }

  private async readManifest(): Promise<ProjectRecord[]> {
    let text: string;
    try {
      text = await readFile(this.filePath, "utf8");
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") {
        this.loaded = [];
        return this.loaded;
      }
      logError("[peri-cf] Project manifest is unreadable", error, { path: this.filePath });
      this.loaded = [];
      return this.loaded;
    }
    try {
      const parsed = parseProjectFile(JSON.parse(text));
      this.loaded = parsed.projects;
      return this.loaded;
    } catch (error) {
      // 清单损坏时不让服务整体不可用；保留文件内容，下一次写入会显式覆盖。
      logError("[peri-cf] Project manifest is invalid", error, { path: this.filePath });
      this.loaded = [];
      return this.loaded;
    }
  }

  /** 串行化读-改-写；失败的任务不阻塞后续写入。 */
  private mutate<T>(
    task: (projects: ProjectRecord[]) => Promise<{ next: ProjectRecord[]; result: T }>,
  ): Promise<T> {
    const run = this.writeChain.then(async () => {
      const current = (await this.load()).map((project) => ({ ...project }));
      const { next, result } = await task(current);
      if (next !== current) await this.persist(next);
      this.loaded = next;
      return result;
    });
    this.writeChain = run.then(() => undefined, () => undefined);
    return run;
  }

  private async persist(projects: ProjectRecord[]): Promise<void> {
    const payload: ProjectFile = { version: PROJECT_FILE_VERSION, projects };
    await mkdir(dirname(this.filePath), { recursive: true });
    const temporary = `${this.filePath}.${randomUUID()}.tmp`;
    await writeFile(temporary, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
    await rename(temporary, this.filePath);
  }
}

export function defaultProjectView(): ProjectView {
  return { id: DEFAULT_PROJECT_ID, name: DEFAULT_PROJECT_NAME, createdAt: "", isDefault: true };
}

export function normalizeProjectName(raw: unknown): string {
  if (typeof raw !== "string") throw new ProjectNameError("项目名称必须是字符串");
  const name = raw.trim().replace(/\s+/g, " ");
  if (name.length === 0) throw new ProjectNameError("项目名称不能为空");
  if (name.length > PROJECT_NAME_MAX_LENGTH) {
    throw new ProjectNameError(`项目名称不能超过 ${PROJECT_NAME_MAX_LENGTH} 个字符`);
  }
  if (/[\u0000-\u001f\u007f]/.test(name)) throw new ProjectNameError("项目名称不能包含控制字符");
  return name;
}

function toView(project: ProjectRecord): ProjectView {
  return { id: project.id, name: project.name, createdAt: project.createdAt, isDefault: false };
}

function parseProjectFile(value: unknown): ProjectFile {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("项目清单必须是对象");
  }
  const record = value as Record<string, unknown>;
  if (record.version !== PROJECT_FILE_VERSION) throw new TypeError("项目清单版本不受支持");
  if (!Array.isArray(record.projects)) throw new TypeError("项目清单 projects 必须是数组");
  const ids = new Set<string>();
  const projects = record.projects.map((entry): ProjectRecord => {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) {
      throw new TypeError("项目条目必须是对象");
    }
    const project = entry as Record<string, unknown>;
    if (typeof project.id !== "string" || project.id.length === 0 || ids.has(project.id)) {
      throw new TypeError("项目 id 无效或重复");
    }
    if (typeof project.name !== "string" || project.name.length === 0) {
      throw new TypeError("项目 name 无效");
    }
    if (project.sandboxId !== null && typeof project.sandboxId !== "string") {
      throw new TypeError("项目 sandboxId 无效");
    }
    if (typeof project.createdAt !== "string") throw new TypeError("项目 createdAt 无效");
    ids.add(project.id);
    return {
      id: project.id,
      name: project.name,
      sandboxId: project.sandboxId as string | null,
      createdAt: project.createdAt,
    };
  });
  return { version: PROJECT_FILE_VERSION, projects };
}
