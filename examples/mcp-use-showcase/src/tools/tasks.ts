import type { MCPServer } from "mcp-use";
import { z } from "zod";
import { incrementTaskRuns } from "../state.ts";

/** 长任务工具：进度、自定义通知与日志。 */
export function registerTasksTools(server: MCPServer): void {
  server.tool(
    {
      name: "slow_task",
      title: "Slow task",
      description:
        "模拟长任务：上报进度、发送自定义通知和 MCP 日志，最后返回结果。",
      inputSchema: z.object({
        steps: z.number().int().min(1).max(10).default(3).describe("步骤数"),
        stepMs: z.number().int().min(0).max(2000).default(200).describe("每步耗时毫秒"),
      }),
      annotations: {
        readOnlyHint: false,
        destructiveHint: false,
      },
    },
    async ({ steps, stepMs }, ctx) => {
      const runId = `run-${incrementTaskRuns()}`;
      const hasProgress = await ctx.reportProgress(0, steps, "starting");
      await ctx.sendNotification("demo/task-status", {
        runId,
        status: "started",
        steps,
        progressSupported: hasProgress,
      });
      await ctx.sendLog("info", { runId, steps, stepMs }, "showcase");

      for (let step = 1; step <= steps; step += 1) {
        if (stepMs > 0) await new Promise((resolve) => setTimeout(resolve, stepMs));
        await ctx.reportProgress(step, steps, `step ${step}/${steps}`);
        await ctx.sendNotification("demo/task-status", {
          runId,
          status: "running",
          completed: step,
          total: steps,
        });
      }

      await ctx.sendNotification("demo/task-status", {
        runId,
        status: "completed",
      });
      return {
        content: [
          {
            type: "text",
            text: `任务 ${runId} 完成：${steps} 步，进度通知支持=${hasProgress}。`,
          },
        ],
      };
    },
  );
}
