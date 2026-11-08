import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, rename, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { createReplayWorld, prepareReplaySessions } from "../../helpers/llm-replay-tui.js";
import { readReplayStore, replayIsolationEnvironment, replayStorePath } from "../../helpers/replay-isolation.js";
import { bodyOf, identity, lastUserText, preserveAndRemoveAuxiliaryWorld, results, sessionModel, sessionPeer, stored, turn } from "./session-store-fixture.js";

async function treeDigest(root: string): Promise<string> {
  const hash = createHash("sha256");
  async function visit(directory: string): Promise<void> {
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      const file = path.join(directory, entry.name); hash.update(path.relative(root, file));
      if (entry.isDirectory()) await visit(file);
      else if (entry.isFile()) hash.update(await readFile(file));
      else throw new Error("unexpected canary entry");
    }
  }
  await visit(root); return hash.digest("hex");
}

describe("replay v3 · temporary session-store isolation", () => {
  it("D01 poisoned host environment cannot redirect real TUI, shell, cache or session persistence", async () => {
    const canary = await mkdtemp(path.join(os.tmpdir(), "peri-replay-host-canary-"));
    const replacements: Record<string, string> = {
      HOME: canary, USERPROFILE: canary, XDG_CONFIG_HOME: path.join(canary, "config"), XDG_CACHE_HOME: path.join(canary, "cache"),
      XDG_DATA_HOME: path.join(canary, "data"), XDG_STATE_HOME: path.join(canary, "state"), XDG_RUNTIME_DIR: path.join(canary, "runtime"),
      ANTHROPIC_API_KEY: "D01_HOST_SECRET_MUST_NOT_LEAK", OPENAI_API_KEY: "D01_OTHER_SECRET_MUST_NOT_LEAK",
      PERI_DB_PATH: path.join(canary, "host-store.db"), PERI_SESSION_STORE: "https://D01-invalid-host-store.test/db",
      BASH_ENV: path.join(canary, "shell-env"), ENV: path.join(canary, "shell-env"),
    };
    await mkdir(path.join(canary, ".peri"));
    await writeFile(path.join(canary, ".peri/settings.json"), '{"config":{"active_alias":"HOST_CANARY_PROVIDER"}}');
    await writeFile(path.join(canary, "host-store.db"), "D01_IMMUTABLE_HOST_STORE");
    await writeFile(path.join(canary, "shell-env"), "printf 'D01_HOST_SHELL_ENV_LEAK' > host-env-leaked.txt\n");
    const before = await treeDigest(canary);
    const previous = Object.fromEntries(Object.keys(replacements).map((key) => [key, process.env[key]]));
    Object.assign(process.env, replacements);
    try {
      await runReplayScenario({ id: "D01", title: "父环境毒化与存储逃逸拒绝，真实 shell 和数据库仍完全隔离", configVersion: "isolated-store-v3-1",
        upstream: async () => sessionModel((body) => {
          if (!results(body).length) {
            const python = 'import json,os,tempfile; from pathlib import Path; keys="HOME USERPROFILE XDG_CONFIG_HOME XDG_CACHE_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_RUNTIME_DIR TMPDIR TMP TEMP PERI_DB_PATH ANTHROPIC_API_KEY OPENAI_API_KEY BASH_ENV ENV PERI_SESSION_STORE".split(); f=tempfile.NamedTemporaryFile(prefix="D01-",delete=False); f.write(b"D01_TEMP_EFFECT"); f.close(); Path("environment.json").write_text(json.dumps({"env":{k:os.environ.get(k) for k in keys},"temp":f.name})); print("D01_REAL_SHELL_COMPLETED")';
            return { tool: { id: "D01-env", name: "Bash", input: { command: `python3 -c '${python}'` } } };
          }
          expect(results(body)).toHaveLength(1); expect(results(body)[0].is_error).not.toBe(true);
          expect(JSON.stringify(results(body)[0].content)).toContain("D01_REAL_SHELL_COMPLETED");
          return { text: "D01_ISOLATED_SESSION_COMPLETE" };
        }), async run(ctx) {
          await expect(ctx.startTui("root", { env: { HOME: canary } })).rejects.toThrow(/isolation rejects HOME/);
          await expect(ctx.startTui("root", { env: { PERI_DB_PATH: path.join(canary, "host-store.db") } })).rejects.toThrow(/isolation rejects PERI_DB_PATH/);
          for (const args of [
            [`--db-path=${path.join(canary, "host-store.db")}`], ["--dbPath", path.join(canary, "host-store.db")],
            ["--config-file", path.join(canary, ".peri/settings.json")], [`--configFile=${path.join(canary, ".peri/settings.json")}`],
            ["--sessionStore=https://D01-invalid-host-store.test/db"], ["--settings", path.join(canary, ".peri/settings.json")],
            ["--no-session-persistence"],
          ]) await expect(ctx.startTui("root", { args })).rejects.toThrow(/isolation rejects storage/);
          const settings = path.join(ctx.world.home, ".peri", "settings.json");
          await rename(settings, `${settings}.isolated`);
          try {
            await symlink(path.join(canary, ".peri", "settings.json"), settings);
            await expect(ctx.startTui("root")).rejects.toThrow(/isolation rejects a redirected/);
          } finally { await rm(settings, { force: true }); await rename(`${settings}.isolated`, settings); }
          const originalSettings = await readFile(settings, "utf8");
          try {
            const injected = JSON.parse(originalSettings); injected.config.env = { BASH_ENV: path.join(canary, "shell-env") };
            await writeFile(settings, JSON.stringify(injected));
            await expect(ctx.startTui("root")).rejects.toThrow(/isolation rejects BASH_ENV/);
          } finally { await writeFile(settings, originalSettings); }
          for (const [file, env] of [
            [path.join(ctx.world.work, ".peri", "settings.json"), { RUST_LOG_FILE: path.join(canary, "external.log") }],
            [path.join(ctx.world.home, ".claude", "settings.json"), { PERI_DB_PATH: path.join(canary, "host-store.db") }],
          ] as const) {
            await mkdir(path.dirname(file), { recursive: true });
            try {
              await writeFile(file, JSON.stringify({ env }));
              await expect(ctx.startTui("root")).rejects.toThrow(/isolation rejects (RUST_LOG_FILE|PERI_DB_PATH)/);
            } finally { await rm(file, { force: true }); }
          }
          expect(ctx.requests).toHaveLength(0); expect(await treeDigest(canary)).toBe(before);
          const tui = await ctx.startTui("root");
          await turn(ctx, tui, "root", "environment", "D01_INSPECT_REAL_TOOL_ENVIRONMENT", "D01_ISOLATED_SESSION_COMPLETE", 2);
          const environment = JSON.parse(await readFile(path.join(ctx.world.work, "environment.json"), "utf8"));
          expect(environment.env).toMatchObject(replayIsolationEnvironment(ctx.world));
          expect(environment.env.PERI_DB_PATH).toBe(replayStorePath(ctx.world));
          for (const key of ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "BASH_ENV", "ENV", "PERI_SESSION_STORE"]) expect(environment.env[key], key).toBeNull();
          expect(path.dirname(environment.temp)).toBe(path.join(ctx.world.home, "tmp"));
          expect(await readFile(environment.temp, "utf8")).toBe("D01_TEMP_EFFECT");
          expect(await treeDigest(canary)).toBe(before);
          expect(JSON.stringify(ctx.requests.map(bodyOf))).not.toMatch(/D01_HOST_SECRET|D01_OTHER_SECRET|HOST_CANARY_PROVIDER/);
          expect(stored(ctx.world, ctx.sessionIds.root!).filter((value) => value.includes("D01_ISOLATED_SESSION_COMPLETE"))).toHaveLength(1);
          await tui.stop(); expect(await treeDigest(canary)).toBe(before);
          ctx.observe("real_child_environment_and_canary", { environment, canaryUnchanged: true, store: readReplayStore(ctx.world) });
        } });
    } finally {
      for (const [key, value] of Object.entries(previous)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
      await rm(canary, { recursive: true, force: true });
    }
  });

  it("D02 eight turns survive four separate terminal generations without duplicates or lost context", async () => {
    await runReplayScenario({ id: "D02", title: "八轮对话跨四代真实进程，历史精确一次且上下文连续",
      upstream: async () => sessionModel((body) => {
        const round = lastUserText(body).match(/D02_INPUT_(\d+)/)?.[1];
        if (!round) throw new Error("missing D02 turn identity"); return { text: `D02_OUTPUT_${round}` };
      }), async run(ctx) {
        let count = 0;
        for (let generation = 0; generation < 4; generation++) {
          const tui = await ctx.startTui("root", { label: `generation-${generation}` });
          if (count) await tui.waitFor((screen) => screen.includes(`D02_OUTPUT_${count}`), "persisted prior generation visible");
          expect(ctx.requests).toHaveLength(count);
          for (let index = 0; index < 2; index++) {
            count++;
            await turn(ctx, tui, "root", `round-${count}`, `D02_INPUT_${count}`, `D02_OUTPUT_${count}`, count);
            const body = JSON.stringify(bodyOf(ctx.requests[count - 1]).messages);
            for (let previous = 1; previous < count; previous++) {
              expect(body.split(`D02_INPUT_${previous}`).length - 1).toBe(1);
              expect(body.split(`D02_OUTPUT_${previous}`).length - 1).toBe(1);
            }
          }
          await tui.stop();
          const rows = stored(ctx.world, ctx.sessionIds.root!);
          for (let previous = 1; previous <= count; previous++) {
            expect(rows.filter((value) => value.includes(`D02_INPUT_${previous}`))).toHaveLength(1);
            expect(rows.filter((value) => value.includes(`D02_OUTPUT_${previous}`))).toHaveLength(1);
          }
          ctx.observe(`generation-${generation}-store`, readReplayStore(ctx.world));
        }
        expect(new Set(ctx.requests.map((request) => request.context.execution)).size).toBe(8);
        expect(new Set(ctx.requests.map((request) => request.context.owner))).toEqual(new Set(["root"]));
      } });
  });

  it("D03 interleaved cold loads of three sessions retain only their own transcript", async () => {
    await runReplayScenario({ id: "D03", title: "同一临时 store 三会话两轮交错冷恢复，界面和模型历史无串扰", sessionNames: ["A", "B", "C"],
      upstream: async () => sessionModel((body) => {
        const marker = lastUserText(body).match(/D03_([ABC])_INPUT_(\d)/);
        if (!marker) throw new Error("missing D03 input"); return { text: `D03_${marker[1]}_OUTPUT_${marker[2]}` };
      }), async run(ctx) {
        let count = 0;
        for (const round of [1, 2]) for (const alias of round === 1 ? ["A", "B", "C"] : ["C", "A", "B"]) {
          const tui = await ctx.startTui(alias, { label: `${alias}-${round}` });
          if (round === 2) await tui.waitFor((screen) => screen.includes(`D03_${alias}_OUTPUT_1`), "own old history visible");
          const screen = await turn(ctx, tui, alias, `round-${round}`, `D03_${alias}_INPUT_${round}`, `D03_${alias}_OUTPUT_${round}`, ++count);
          const request = JSON.stringify(bodyOf(ctx.requests.at(-1)).messages);
          for (const other of ["A", "B", "C"].filter((value) => value !== alias)) {
            expect(screen).not.toContain(`D03_${other}_`); expect(request).not.toContain(`D03_${other}_`);
          }
          if (round === 2) expect(request).toContain(`D03_${alias}_OUTPUT_1`);
          await tui.stop();
        }
        const store = readReplayStore(ctx.world); expect(store.integrity).toBe("ok"); expect(store.threads).toHaveLength(3);
        for (const alias of ["A", "B", "C"]) {
          const rows = stored(ctx.world, ctx.sessionIds[alias]!);
          expect(rows.filter((value) => value.includes(`D03_${alias}_INPUT_`))).toHaveLength(2);
          expect(rows.filter((value) => value.includes(`D03_${alias}_OUTPUT_`))).toHaveLength(2);
          for (const other of ["A", "B", "C"].filter((value) => value !== alias)) expect(rows.join()).not.toContain(`D03_${other}_`);
        }
        ctx.observe("three_session_store", store);
      } });
  });

  it("D04 closing one process does not close another process's session-store or active turn", async () => {
    await runReplayScenario({ id: "D04", title: "同 store 双进程独立会话并发，关闭一方后另一方仍提交", sessionNames: ["A", "B"],
      upstream: async () => sessionModel((body) => ({ text: lastUserText(body).includes("D04_A_INPUT") ? "D04_A_COMMITTED" : "D04_B_COMMITTED" })),
      runOptions: () => ({ gates: ["A", "B"].map((owner) => ({ identity: identity(owner, "parallel"), afterBytes: 0 })) }),
      async run(ctx) {
        const a = await ctx.startTui("A"), b = await ctx.startTui("B");
        ctx.identity.arm(ctx.sessionIds.A!, "parallel"); ctx.identity.arm(ctx.sessionIds.B!, "parallel");
        await Promise.all([a.prompt("D04_A_INPUT"), b.prompt("D04_B_INPUT")]);
        await Promise.all([ctx.run.waitReached(identity("A", "parallel")), ctx.run.waitReached(identity("B", "parallel"))]);
        const held = ctx.proxy.snapshot(ctx.runId);
        expect(held.calls).toHaveLength(2);
        expect(held.calls.filter((call: any) => ["reserved", "streaming"].includes(call.state))).toHaveLength(2);
        ctx.run.release(identity("B", "parallel"));
        await b.waitFor(ctx.checked((screen: string) => screen.includes("D04_B_COMMITTED") && screen.includes("Brewed for")), "B commits while A remains active");
        await expect.poll(() => stored(ctx.world, ctx.sessionIds.B!).filter((value) => value.includes("D04_B_COMMITTED")).length).toBe(1);
        expect(stored(ctx.world, ctx.sessionIds.A!).join()).not.toContain("D04_A_COMMITTED");
        ctx.identity.finish(ctx.sessionIds.B!); await b.save("B-before-close"); await b.stop();
        const remaining = ctx.proxy.snapshot(ctx.runId).calls.filter((call: any) => ["reserved", "streaming"].includes(call.state));
        expect(remaining).toHaveLength(1); expect(remaining[0].identity.owner).toBe("A");
        ctx.run.release(identity("A", "parallel"));
        await a.waitFor(ctx.checked((screen: string) => screen.includes("D04_A_COMMITTED") && screen.includes("Brewed for")), "A still completes after B deployment closes");
        await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length === 2);
        ctx.identity.finish(ctx.sessionIds.A!); await a.save("A-after-B-close"); await a.stop();
        const reopened = await ctx.startTui("B", { label: "B-reopened" });
        const screen = await reopened.waitFor((value) => value.includes("D04_B_COMMITTED"), "B durable history reopens");
        expect(screen).not.toContain("D04_A_COMMITTED"); expect(ctx.requests).toHaveLength(2);
        expect(stored(ctx.world, ctx.sessionIds.A!).filter((value) => value.includes("D04_A_COMMITTED"))).toHaveLength(1);
        ctx.observe("store_after_both_deployments", readReplayStore(ctx.world));
      } });
  });

  it("D05 a session ID from another temporary store fails exactly and never issues a model request", async () => {
    await runReplayScenario({ id: "D05", title: "跨临时 store 冒用 session ID 精确拒绝，原会话仍可冷恢复续聊",
      upstream: async () => sessionModel((body) => ({ text: lastUserText(body).includes("D05_CONTINUE") ? "D05_ORIGINAL_CONTINUED" : "D05_PRIVATE_ORIGINAL_HISTORY" })),
      async run(ctx) {
        let tui = await ctx.startTui("root");
        await turn(ctx, tui, "root", "original", "D05_ORIGINAL_INPUT", "D05_PRIVATE_ORIGINAL_HISTORY", 1); await tui.stop();
        const other = await createReplayWorld();
        try {
          const [otherId] = await prepareReplaySessions(other);
          expect(otherId).not.toBe(ctx.sessionIds.root);
          const peer = await sessionPeer(other, { proxy: ctx.proxy.url, run: ctx.runId });
          try {
            const response = await peer.call("session/load", { sessionId: ctx.sessionIds.root, cwd: other.work, mcpServers: [] });
            // Cold resource lookup maps typed NotFound through workspace::resource_error.
            expect(response.error).toEqual({ code: -32010, message: "session not found (NotApplied)" });
            expect(JSON.stringify(peer.notifications)).not.toContain("D05_PRIVATE_ORIGINAL_HISTORY");
            expect(ctx.requests).toHaveLength(1);
          } finally { expect(await peer.close()).toBe(0); }
          const store = readReplayStore(other);
          expect(store.integrity).toBe("ok"); expect(store.threads.map((thread) => thread.id)).toEqual([otherId]);
          expect(store.messages).toEqual([]); ctx.observe("foreign_store_rejects_original", store);
        } finally { await preserveAndRemoveAuxiliaryWorld(ctx, other, "foreign-store"); }
        tui = await ctx.startTui("root", { label: "original-reopened" });
        await tui.waitFor((screen) => screen.includes("D05_PRIVATE_ORIGINAL_HISTORY"), "original store unaffected");
        await turn(ctx, tui, "root", "continue", "D05_CONTINUE", "D05_ORIGINAL_CONTINUED", 2);
        expect(JSON.stringify(bodyOf(ctx.requests[1]).messages)).toContain("D05_PRIVATE_ORIGINAL_HISTORY");
      } });
  });
});
