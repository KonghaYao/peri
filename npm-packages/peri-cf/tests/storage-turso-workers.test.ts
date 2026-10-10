import { expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { TursoStorage, SESSION_BY_ID_SQL, SESSION_LIST_SQL, type SessionStorage, type SessionSummary } from "../worker/sdk";
import { chatRepository } from "../worker/chat/repository";
import type { Env } from "../worker/types";

test("Store access stays with the caller: explicit url plus optional token, no deployment or environment fallback", () => {
  const storage: SessionStorage = new TursoStorage({ url: "https://fixture.invalid", authToken: "fixture-explicit" });
  expect(storage).toHaveProperty("getSessions");
  expect(storage).toHaveProperty("getSession");
  // 部署参数（engine / tokenEnv / deployment()）与隐式环境凭证已从 SDK 契约移除：
  // Store 位置与凭证由调用方显式传递，peri-cf 从部署环境读取后交给 WASM 启动对象。
  expect("deployment" in storage).toBe(false);
});

test("the app Store boundary still refuses to query without explicit deployment configuration", async () => {
  const repository = chatRepository({} as Env, async () => "session");
  await expect(repository.list()).rejects.toThrow("Turso storage is not configured");
  await expect(repository.get("session")).rejects.toThrow("Turso storage is not configured");
});

test("bundled Workers TursoStorage reads canonical metadata over native fetch without Bun or process globals", async () => {
  const expected: SessionSummary = { id: "fixture-session", title: null, cwd: "/fixture-workspace", messageCount: 3,
    createdAt: "fixture-created", updatedAt: "fixture-updated" };
  const queries: Array<{ sql: string; parameter: string; authorization: string | null }> = [];
  let closes = 0;
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, async fetch(request) {
    const payload = await request.json() as any;
    if (new URL(request.url).pathname === "/v3/pipeline") {
      const results = payload.requests.map((entry: { type: string }) => {
        if (entry.type === "close") closes++;
        return { type: "ok", response: entry.type === "get_autocommit" ? { type: "get_autocommit", is_autocommit: true } : { type: "close" } };
      });
      return Response.json({ baton: payload.requests.some((entry: { type: string }) => entry.type === "close") ? null : payload.baton, results });
    }
    const statement = payload.batch.steps[0].stmt;
    const parameter = statement.args[0].value;
    queries.push({ sql: statement.sql, parameter, authorization: request.headers.get("Authorization") });
    const entries: unknown[] = [{ baton: "fixture-baton", base_url: null }];
    if (parameter === "fresh" || parameter === "failure") {
      entries.push({ type: "step_error", step: 0, error: { code: "SQLITE_ERROR",
        message: parameter === "fresh" ? "no such table: threads" : "fixture query failed" } });
    } else {
      entries.push({ type: "step_begin", step: 0, cols: ["id", "title", "cwd", "message_count", "created_at", "updated_at"].map(name => ({ name })) });
      if (parameter !== "absent") entries.push({ type: "row", row: [
        { type: "text", value: expected.id }, { type: "null" }, { type: "text", value: expected.cwd },
        { type: "integer", value: "3" }, { type: "text", value: expected.createdAt }, { type: "text", value: expected.updatedAt },
      ] });
      entries.push({ type: "step_end", step: 0, affected_row_count: 0 });
    }
    entries.push({ type: "step_begin", step: 1, cols: [] }, { type: "step_end", step: 1, affected_row_count: 0 });
    return new Response(entries.map(entry => JSON.stringify(entry)).join("\n") + "\n");
  } });
  const directory = await mkdtemp(join(tmpdir(), "peri-turso-workers-"));
  try {
    const bundle = await Bun.build({ entrypoints: [new URL("../worker/sdk/index.ts", import.meta.url).pathname], target: "bun",
      format: "esm", external: ["node:crypto"] });
    expect(bundle.success).toBe(true);
    const source = await bundle.outputs[0]!.text();
    const path = join(directory, "workers.mjs");
    await writeFile(path, source);
    const script = `
      import assert from "node:assert/strict";
      const host = process;
      delete globalThis.localStorage;
      delete globalThis.sessionStorage;
      assert.equal(typeof globalThis.Bun, "undefined");
      const { TursoStorage } = await import(${JSON.stringify(pathToFileURL(path).href)});
      const url = ${JSON.stringify(server.url.origin)};
      const expected = ${JSON.stringify(expected)};
      const preferred = new TursoStorage({ url, authToken: "fixture-explicit" });
      assert.deepEqual(await preferred.getSession(expected.id), expected);
      globalThis.process = undefined;
      const explicit = new TursoStorage({ url, authToken: "fixture-worker" });
      assert.deepEqual(await explicit.getSessions(expected.cwd), [expected]);
      assert.deepEqual(await explicit.getSession(expected.id), expected);
      assert.equal(await explicit.getSession("absent"), null);
      assert.deepEqual(await explicit.getSessions("fresh"), []);
      assert.equal(await explicit.getSession("fresh"), null);
      await assert.rejects(explicit.getSessions("failure"), /fixture query failed/);
      await assert.rejects(explicit.getSession("failure"), /fixture query failed/);
      // 没有显式 token 时不得回退到宿主环境变量：匿名读取保持无 Authorization 头。
      const anonymous = new TursoStorage({ url });
      assert.equal(await anonymous.getSession("absent"), null);
      host.stdout.write("Workers native-fetch metadata checks passed");
    `;
    const child = Bun.spawn(["node", "--input-type=module", "-e", script], { stdout: "pipe", stderr: "pipe" });
    const [status, stdout, stderr] = await Promise.all([child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()]);
    expect({ status, stderr }).toEqual({ status: 0, stderr: "" });
    expect(stdout).toBe("Workers native-fetch metadata checks passed");
    expect(queries).toHaveLength(9);
    expect(closes).toBe(queries.length);
    expect(queries.every(query => query.sql === SESSION_LIST_SQL || query.sql === SESSION_BY_ID_SQL)).toBe(true);
    expect(queries.filter(query => query.sql === SESSION_LIST_SQL).map(query => query.parameter))
      .toEqual([expected.cwd, "fresh", "failure"]);
    expect(queries.map(query => query.authorization)).toEqual([
      "Bearer fixture-explicit",
      ...Array<string>(7).fill("Bearer fixture-worker"), null,
    ]);
  } finally {
    server.stop(true);
    await rm(directory, { recursive: true, force: true });
  }
}, 10000);
