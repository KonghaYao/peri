import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { runReplayScenario } from "../../helpers/llm-replay-suite.js";
import { agentProvider, expectResult, resultText, text, tool, tools, type Reply } from "../replay-v2/agents-fixture.js";
import {
  ShellBindings, assertReceipt, assertStartedOnce, configureShell, fileReady, finished, gatedCommand,
  programStatuses, releaseShell, requestText, resultFromBody, startShellTui,
} from "./async-shell-fixture.js";

const outputCases = [
  { id: "H01", title: "后台 Shell 在父回合结束后完成、唤醒与真实 Read，Shell 不占用 OSC working", mode: "success" },
  { id: "H02", title: "后台 Shell 非零退出保留 stderr、exit code 与单次副作用", mode: "failure" },
  { id: "H03", title: "超过 2 MiB Unicode 后台日志完整落盘，通知只带引用并真实读取尾部", mode: "large" },
  { id: "H04", title: "前台超时提升为后台，原进程及输出连续且不会启动第二份", mode: "promotion" },
] as const;

describe("H01–H04 async shell output and ownership lifecycle", () => {
  for (const entry of outputCases) {
    it(`${entry.id} ${entry.title}`, { timeout: 120_000 }, async () => {
      const name = entry.id.toLowerCase();
      const stdout = `${entry.id}_ACTUAL_STDOUT\n`, stderr = `${entry.id}_ACTUAL_STDERR\n`;
      const tail = `${entry.id}_UNICODE_TAIL_终点`;
      const line = `${entry.id}_PAYLOAD_${"文界🧪".repeat(65)}\n`;
      const payload = `${line.repeat(4000)}${tail}\n`;
      const suffix = entry.mode === "large" ? "cat payload.txt" : `printf '${stdout.replace(/\n/g, "\\n")}'`;
      const command = gatedCommand(name, `${suffix}; printf '${stderr.replace(/\n/g, "\\n")}' >&2; printf '${entry.id}_SIDE_EFFECT\\n' > ${name}.result; exit ${entry.mode === "failure" ? 7 : 0}`);
      const spec = { id: `${entry.id}-bash`, alias: name, command };
      let binding: ShellBindings;
      await runReplayScenario({
        id: entry.id, title: entry.title, configVersion: `shell-output-${entry.id}-1`,
        async configure(world) {
          await configureShell(world);
          if (entry.mode === "large") await writeFile(path.join(world.work, "payload.txt"), payload);
        },
        upstream: async () => agentProvider((call, body): Reply => {
          if (call.execution === "start" && call.call === "reason:1") return tools(tool(spec.id, "Bash", {
            command, ...(entry.mode === "promotion" ? { timeout: 1, run_in_background: false } : { run_in_background: true }),
          }));
          if (call.execution === "start" && call.call === "reason:2") {
            expectResult(body, spec.id, "Background task started: mcp-");
            return text(`${entry.id}_PARENT_IDLE`);
          }
          if (call.execution === "start" && call.call === "reason:3") {
            const result = resultFromBody(body, binding.task(name));
            expect(result).toBeDefined();
            expect(result!.state).toBe(entry.mode === "failure" ? "failed" : "succeeded");
            expect(result!.exitCode).toBe(entry.mode === "failure" ? "7" : "0");
            expect(requestText(body)).not.toContain(entry.mode === "large" ? tail : stdout.trim());
            return tools(tool(`${entry.id}-read-stdout`, "Read", {
              file_path: result!.stdout!, ...(entry.mode === "large" ? { offset: 4001, limit: 1 } : {}),
            }));
          }
          if (call.execution === "start" && call.call === "reason:4") {
            expectResult(body, `${entry.id}-read-stdout`, entry.mode === "large" ? tail : stdout.trim());
            return tools(tool(`${entry.id}-read-stderr`, "Read", { file_path: binding.output(name).stderr! }));
          }
          if (call.execution === "start" && call.call === "reason:5") {
            expectResult(body, `${entry.id}-read-stderr`, stderr.trim());
            return text(`${entry.id}_SHELL_VERIFIED`);
          }
          throw new Error(`unexpected ${entry.id} call ${JSON.stringify(call)}`);
        }),
        runOptions(ctx) { binding = new ShellBindings(ctx, [spec]); return { onRequest: binding.onRequest }; },
        async run(ctx) {
          const tui = await startShellTui(ctx);
          ctx.identity.arm(ctx.sessionIds.root!, "start");
          await tui.prompt(`Run ${entry.id} background shell.`);
          const before = await finished(ctx, tui, `${entry.id}_PARENT_IDLE`);
          expect(before).toMatch(/Shell/u);
          await assertReceipt(ctx, spec.id);
          const pid = Number((await fileReady(ctx, `${name}.pid`)).trim());
          expect(pid).toBeGreaterThan(0);
          await assertStartedOnce(ctx, name);
          if (entry.id === "H01") {
            const statuses = await programStatuses(tui, "parent-idle-shell-still-live");
            expect(statuses.at(-1)).toBe("idle");
            await tui.command("tasks");
            const panel = await tui.waitFor(ctx.checked((screen: string) => screen.includes("Background") && screen.includes(binding.task(name))), "real live Shell row in Tasks panel");
            expect(panel).toContain("1");
            await tui.save("live-shell-task-panel");
            await tui.key("Escape");
          }
          await releaseShell(ctx, name);
          const after = await finished(ctx, tui, `${entry.id}_SHELL_VERIFIED`);
          expect(after).toContain("Read");
          await ctx.waitForCalls((snapshot) => snapshot.active === 0 && snapshot.calls.length >= 5);
          ctx.identity.finish(ctx.sessionIds.root!);
          const result = binding.output(name);
          expect(result.state).toBe(entry.mode === "failure" ? "failed" : "succeeded");
          expect(result.exitCode).toBe(entry.mode === "failure" ? "7" : "0");
          expect(await readFile(result.stdout!, "utf8")).toBe(entry.mode === "large" ? payload : stdout);
          expect(await readFile(result.stderr!, "utf8")).toBe(stderr);
          if (entry.mode === "large") expect(Buffer.byteLength(payload)).toBeGreaterThan(2 * 1024 * 1024);
          expect(await readFile(path.join(ctx.world.work, `${name}.result`), "utf8")).toBe(`${entry.id}_SIDE_EFFECT\n`);
          await assertStartedOnce(ctx, name);
          const reads = ctx.requests.filter((request) => ["reason:3", "reason:4", "reason:5"].includes(request.context.call));
          expect(reads.filter((request) => request.context.call === "reason:3")).toHaveLength(1);
          expect(resultText(JSON.parse(reads.at(-1)!.request.body.toString()), `${entry.id}-read-stdout`)).toContain(entry.mode === "large" ? tail : stdout.trim());
          ctx.observe("shell-independent-oracle", { pid, task: binding.task(name), state: result.state, exitCode: result.exitCode,
            stdoutBytes: Buffer.byteLength(entry.mode === "large" ? payload : stdout), stderr, starts: 1, completionRuns: 1 });
          await tui.save("shell-output-verified");
        },
      });
    });
  }
});

it("H11 full-async-tools changes Bash's default while explicit false remains foreground", { timeout: 120_000 }, async () => {
  const command = gatedCommand("h11", "printf 'H11_BACKGROUND_FILE\\n' > h11.background; printf 'H11_BG_STDOUT\\n'");
  const spec = { id: "H11-default", alias: "h11", command };
  let binding: ShellBindings;
  await runReplayScenario({ id: "H11", title: "full-async-tools 下 Bash 省略参数走后台，显式 false 保持前台", configVersion: "bash-beta-1",
    async configure(world) {
      await configureShell(world);
      const file = path.join(world.home, ".peri/settings.json");
      const config = JSON.parse(await readFile(file, "utf8"));
      config.config.betas = { "full-async-tools": true };
      await writeFile(file, JSON.stringify(config));
    },
    upstream: async () => agentProvider((call, body): Reply => {
      if (call.execution === "start" && call.call === "reason:1") return tools(tool(spec.id, "Bash", { command }));
      if (call.execution === "start" && call.call === "reason:2") {
        expectResult(body, spec.id, "Background task started: mcp-");
        return tools(tool("H11-foreground", "Bash", { command: "printf 'H11_EXPLICIT_FOREGROUND\\n'; printf 'H11_FG_FILE\\n' > h11.foreground", run_in_background: false }));
      }
      if (call.execution === "start" && call.call === "reason:3") {
        expectResult(body, "H11-foreground", "H11_EXPLICIT_FOREGROUND");
        expect(resultText(body, "H11-foreground")).not.toContain("Background task started:");
        return text("H11_DEFAULTS_VERIFIED");
      }
      if (call.execution === "start" && call.call === "reason:4") {
        expect(resultFromBody(body, binding.task("h11"))!.state).toBe("succeeded");
        return text("H11_BACKGROUND_SETTLED");
      }
      throw new Error(`unexpected H11 ${JSON.stringify(call)}`);
    }),
    runOptions(ctx) { binding = new ShellBindings(ctx, [spec]); return { onRequest: binding.onRequest }; },
    async run(ctx) {
      const tui = await startShellTui(ctx);
      ctx.identity.arm(ctx.sessionIds.root!, "start"); await tui.prompt("Run H11 Bash defaults.");
      await finished(ctx, tui, "H11_DEFAULTS_VERIFIED");
      expect(await readFile(path.join(ctx.world.work, "h11.foreground"), "utf8")).toBe("H11_FG_FILE\n");
      const body = JSON.parse(ctx.requests[0]!.request.body.toString());
      expect(body.tools.find((entry: any) => entry.name === "Bash").input_schema.properties.run_in_background.default).toBe(true);
      const statuses = await programStatuses(tui, "default-background-explicit-foreground");
      expect(statuses.at(-1)).toBe("idle");
      await releaseShell(ctx, "h11");
      await finished(ctx, tui, "H11_BACKGROUND_SETTLED"); ctx.identity.finish(ctx.sessionIds.root!);
      expect(await readFile(path.join(ctx.world.work, "h11.background"), "utf8")).toBe("H11_BACKGROUND_FILE\n");
      await assertStartedOnce(ctx, "h11");
      ctx.observe("beta-default", { schemaDefault: true, omitted: "background", explicitFalse: "foreground", tasks: binding.tasks.size });
    },
  });
});
