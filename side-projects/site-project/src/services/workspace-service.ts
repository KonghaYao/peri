// ============ 工作区业务逻辑层 ============
import { workspaceState, deepMerge, saveWorkspaceFile, getWorkspaceKey, setWorkspaceKey } from "../lib/workspace.js";
import type { TerminalSessionMeta } from "../types.js";

export class WorkspaceService {
  constructor(
    private terminalSessions: Map<string, { alive: boolean; cols: number; rows: number; createdAt: number }>,
  ) {}

  getState() {
    const terminalList: TerminalSessionMeta[] = [];
    this.terminalSessions.forEach((s, id) => {
      if (s.alive) terminalList.push({ id, cols: s.cols, rows: s.rows, createdAt: s.createdAt });
    });
    return { ...workspaceState, terminals: terminalList };
  }

  updateState(body: any) {
    const newState = deepMerge(workspaceState, body) as typeof workspaceState;
    Object.assign(workspaceState, newState);
    saveWorkspaceFile(workspaceState);
    return { success: true };
  }

  getKey(key: string) {
    return getWorkspaceKey(key);
  }

  setKey(key: string, patch: any) {
    return setWorkspaceKey(key, patch);
  }
}
