/**
 * 可移植的 Workspace 引用。
 *
 * 纯值对象，不依赖 Node/Bun/浏览器 API。`runtimePath` 是 Agent 与 Session
 * 看到的根（必须是绝对的规范化路径）；`hostPath` 是宿主文件系统中的路径，
 * 远程 sandbox 没有本地宿主路径时可以省略。Sandbox 与 Chat 共享该结构，
 * 因此不放在 `chat/` 或 `sandbox/` 内，避免两侧互相依赖。
 */
export interface WorkspaceRef {
    id: string;
    hostPath?: string;
    runtimePath: string;
}
