/**
 * 笔记领域状态。
 *
 * mcp-use 2.x 的 handler 是 stateless 的：每个请求都会新建 SDK server，
 * 因此跨请求状态只能放在模块作用域，不能放进回调闭包或请求上下文。
 */

export type Note = {
  id: string;
  title: string;
  body: string;
  tags: string[];
  updatedAt: string;
};

export const seedNotes: Note[] = [
  {
    id: "mcp-basics",
    title: "MCP 三个核心原语",
    body: "tools 负责动作，resources 负责只读内容，prompts 负责可复用指令模板。",
    tags: ["mcp", "basics"],
    updatedAt: "2026-01-05T09:00:00.000Z",
  },
  {
    id: "stateless",
    title: "Stateless 服务端约束",
    body: "每个请求都是独立实例，不要把会话状态放在回调闭包里。",
    tags: ["mcp-use", "architecture"],
    updatedAt: "2026-01-06T10:30:00.000Z",
  },
  {
    id: "peri",
    title: "在 Peri 中试用",
    body: "先启动 dev server，再让 Peri 通过 .mcp.json 的 url 连接。",
    tags: ["peri", "integration"],
    updatedAt: "2026-01-07T08:15:00.000Z",
  },
];

const notes = new Map(seedNotes.map((note) => [note.id, note]));

function now(): string {
  return new Date().toISOString();
}

export function listNotes(): Note[] {
  return [...notes.values()].sort((a, b) => a.id.localeCompare(b.id));
}

export function notePayload(id: string): Note | undefined {
  return notes.get(id);
}

export function noteCount(): number {
  return notes.size;
}

/** 写入或覆盖一条笔记，返回是否新建。 */
export function upsertNote(id: string, title: string, body: string, tags: string[]): boolean {
  const created = !notes.has(id);
  notes.set(id, { id, title, body, tags, updatedAt: now() });
  return created;
}

export function resetNotes(): void {
  notes.clear();
  for (const note of seedNotes) notes.set(note.id, note);
}
