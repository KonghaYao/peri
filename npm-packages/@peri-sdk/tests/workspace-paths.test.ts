import { describe, expect, test } from "bun:test";
import { workspaceRelation } from "../src/chat/workspace";
import { resolveChatWorkspace } from "../src/chat/workspace-config";
import { WorkspaceMismatchError } from "../src/workspace/errors";
import {
    normalizeAbsolutePath,
    normalizePath,
    resolveWithin,
} from "../src/workspace/paths";

describe("workspace paths", () => {
    test("normalizes separators, dot segments and drive letters", () => {
        expect(normalizePath("/a//b/./c/../d/")).toBe("/a/b/d");
        expect(normalizePath("C:\\Users\\x\\..\\y")).toBe("c:/Users/y");
        expect(normalizePath("../a")).toBe("../a");
        expect(normalizePath("")).toBeNull();
        expect(normalizeAbsolutePath("/a/../..")).toBe("/");
        expect(normalizeAbsolutePath("relative")).toBeNull();
        expect(normalizeAbsolutePath("c:\\work")).toBe("c:/work");
    });

    test("resolveWithin keeps descendants and rejects absolute paths or escapes", () => {
        expect(resolveWithin("/root", "")).toBe("/root");
        expect(resolveWithin("/root/", "sub/dir")).toBe("/root/sub/dir");
        expect(resolveWithin("/root", "sub/../child")).toBe("/root/child");
        expect(resolveWithin("/root", "sub/..")).toBe("/root");
        expect(resolveWithin("/root", "../other")).toBeNull();
        expect(resolveWithin("/root", "sub/../../other")).toBeNull();
        expect(resolveWithin("/root", "/other")).toBeNull();
        expect(resolveWithin("relative", "sub")).toBeNull();
        expect(resolveWithin("c:/", "sub/dir")).toBe("c:/sub/dir");
        expect(resolveWithin("c:/work", "../other")).toBeNull();
    });

    test("workspaceRelation compares descendants on POSIX and Windows roots", () => {
        expect(workspaceRelation("/root", "/root/sub")).toBe("within");
        expect(workspaceRelation("/root", "/workspace-other")).toBe("outside");
        expect(workspaceRelation("c:/", "c:/work/sub")).toBe("within");
        expect(workspaceRelation("c:/work", "c:/workspace-other")).toBe(
            "outside",
        );
        expect(workspaceRelation("/root", "relative")).toBe("unknown");
    });

    test("resolveChatWorkspace derives a frozen WorkspaceRef and session cwd", () => {
        const resolved = resolveChatWorkspace(
            {
                id: "ws-1",
                hostPath: "/host//ws/./1",
                runtimePath: "/runtime/ws/1/",
            },
            "sub/../project",
        );

        expect(resolved).toEqual({
            workspace: {
                id: "ws-1",
                hostPath: "/host/ws/1",
                runtimePath: "/runtime/ws/1",
            },
            runtimeRoot: "/runtime/ws/1",
            sessionCwd: "/runtime/ws/1/project",
        });
        expect(Object.isFrozen(resolved.workspace)).toBe(true);
    });

    test("resolveChatWorkspace rejects escapes, relative roots and invalid host paths", () => {
        const workspace = { id: "ws-1", runtimePath: "/runtime" };

        expect(() => resolveChatWorkspace(workspace, "../other")).toThrow(
            WorkspaceMismatchError,
        );
        expect(() => resolveChatWorkspace(workspace, "/absolute")).toThrow(
            WorkspaceMismatchError,
        );
        expect(() =>
            resolveChatWorkspace({ id: "ws-1", runtimePath: "relative" }),
        ).toThrow(/runtimePath must be an absolute path/);
        expect(() =>
            resolveChatWorkspace({
                id: "ws-1",
                hostPath: "relative",
                runtimePath: "/runtime",
            }),
        ).toThrow(/hostPath must be an absolute path/);
    });
});
