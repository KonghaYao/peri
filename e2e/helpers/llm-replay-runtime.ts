import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { realpath } from "node:fs/promises";
import type { ReplayWorld } from "./llm-replay-tui.js";

interface AmbientBindingTarget {
  declareAndBind(input: { kind: string; alias: string; live: string; anchor: string }): unknown;
}

/**
 * 登记 world 路径与宿主环境快照。
 *
 * 路径按 realpath 归一：Peri 渲染 cwd 时解析符号链接，macOS 的 /var 与 /private/var
 * 若不统一会表现为两套路径。只在绑定层归一，不改变 world 的创建方式与 fixture 指纹。
 */
export async function declareReplayAmbientBindings(bindings: AmbientBindingTarget, world: ReplayWorld): Promise<void> {
  for (const alias of ["root", "home", "work"] as const) {
    bindings.declareAndBind({ kind: "path", alias, live: await realpath(world[alias]), anchor: `isolated scenario ${alias}` });
  }
  bindings.declareAndBind({ kind: "date", alias: "today", live: new Date().toISOString().slice(0, 10), anchor: "ambient UTC date in system environment" });
  const ambient = replayRuntimeEnvironment();
  bindings.declareAndBind({ kind: "platform", alias: "platform", live: ambient.platform, anchor: "ambient OS platform in system environment" });
  bindings.declareAndBind({ kind: "os_version", alias: "os_version", live: ambient.osVersion, anchor: "ambient OS version in system environment" });
}

/** 按 Peri `PromptRuntimeEnv::detect` 的同一来源读取宿主平台与 OS 版本。 */
export function replayRuntimeEnvironment(): { platform: string; osVersion: string } {
  const platform = process.platform === "darwin" ? "macos" : process.platform === "win32" ? "windows" : process.platform;
  if (process.platform === "linux") {
    try {
      for (const line of readFileSync("/etc/os-release", "utf8").split("\n")) {
        if (line.startsWith("PRETTY_NAME=")) {
          const value = line.slice("PRETTY_NAME=".length).replace(/^"+|"+$/g, "");
          if (value) return { platform, osVersion: value };
        }
      }
    } catch { /* 与 Peri 一样回退到裸标签 */ }
    return { platform, osVersion: "Linux" };
  }
  if (process.platform === "darwin") {
    try {
      const version = execFileSync("sw_vers", ["-productVersion"], { encoding: "utf8" }).trim();
      if (version) return { platform, osVersion: `macOS ${version}` };
    } catch { /* 与 Peri 一样回退到裸标签 */ }
    return { platform, osVersion: "macOS" };
  }
  return { platform, osVersion: platform };
}
