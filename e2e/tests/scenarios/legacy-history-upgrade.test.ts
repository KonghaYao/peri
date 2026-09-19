/** Real old-schema database → ACP → TUI. No model or judge requests. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { execFileSync } from "node:child_process";
import { TmuxTester } from "tui-tester";
import { PROJECT_ROOT, sendPrompt } from "../../helpers/peri.js";

/**
 * 写打开时把旧库升级到的目标版本，事实源是 `schema.rs` 里的 `PRAGMA user_version`
 * （设计 §8：读写打开在同一事务内升级；只读打开不升级）。旧库升级到「当前版本」
 * 是契约本身，写死字面量会在下一次 schema 升版时变成假失败。
 */
const CURRENT_SCHEMA_VERSION = (() => {
  const source = fs.readFileSync(
    path.join(PROJECT_ROOT, "peri-resources/src/sessions/sqlite_store/schema.rs"),
    "utf8",
  );
  const match = /PRAGMA user_version = (\d+)/.exec(source);
  if (!match) throw new Error("schema.rs 未声明 PRAGMA user_version");
  return Number(match[1]);
})();

describe("legacy history upgrade", () => {
  let directory: string;
  let saved: string;
  let database: string;
  let settings: string;
  let oldId: string;
  let missingId: string;
  let tester: TmuxTester | undefined;

  beforeEach(() => {
    directory = fs.mkdtempSync(path.join(os.tmpdir(), "peri-history-upgrade-"));
    saved = path.join(directory, "saved-project");
    fs.mkdirSync(saved);
    fs.mkdirSync(path.join(directory, ".peri/threads"), { recursive: true });
    fs.mkdirSync(path.join(directory, ".cargo"));
    fs.writeFileSync(path.join(directory, ".cargo/env"), "");
    database = path.join(directory, ".peri/threads/threads.db");
    settings = path.join(directory, ".peri/settings.json");
    fs.writeFileSync(settings, JSON.stringify({ config: {
      language: "en", active_alias: "sonnet", show_cache_warning: false,
      providers: [{ id: "local-only", type: "openai", apiKey: "unused-test-key",
        baseUrl: "http://127.0.0.1:9/v1", models: { sonnet: "unused", haiku: "unused", opus: "unused", fable: "unused" } }],
      profiles: Object.fromEntries(["sonnet", "haiku", "opus", "fable"].map(name => [name, { provider: "local-only" }])),
    }}));
    const db = new DatabaseSync(database);
    db.exec(fs.readFileSync(path.join(PROJECT_ROOT, "peri-resources/src/sessions/sqlite_store/fixtures/legacy_with_goals.sql"), "utf8"));
    const seed = (cwd: string, title: string, content: string, updated: string) => {
      const id = randomUUID();
      const messageId = randomUUID();
      db.prepare("INSERT INTO threads (id,title,cwd,created_at,updated_at,message_count) VALUES (?,?,?,'2026-09-01T00:00:00Z',?,1)").run(id, title, cwd, updated);
      db.prepare("INSERT INTO messages (message_id,thread_id,role,content) VALUES (?,?,'user',?)").run(messageId, id, JSON.stringify({ role: "user", id: messageId, content }));
      return id;
    };
    oldId = seed(saved, "LEGACY_SAVED_PROJECT", "LEGACY_RESTORED_MESSAGE", "2026-09-01T00:00:00Z");
    missingId = seed(path.join(directory, "deleted-project"), "LEGACY_DELETED_PROJECT", "LEGACY_READ_ONLY_MESSAGE", "2026-09-02T00:00:00Z");
    db.close();
  });

  afterEach(async () => {
    try { if (tester?.isRunning()) await tester.stop(); }
    finally {
      tester = undefined;
      fs.rmSync(directory, { recursive: true, force: true });
    }
  });

  async function launch(args: string[] = [], cwd = saved) {
    tester = new TmuxTester({
      command: [path.join(PROJECT_ROOT, "target/debug/peri"), `--config-file=${settings}`, ...args],
      cwd, size: { cols: 140, rows: 45 },
      env: {
        HOME: directory, XDG_CONFIG_HOME: path.join(directory, "config"),
        XDG_CACHE_HOME: path.join(directory, "cache"), XDG_DATA_HOME: path.join(directory, "data"),
        LANG: "en_US.UTF-8", LC_ALL: "en_US.UTF-8", TERM: "xterm-256color",
        RUST_LOG_FILE: path.join(directory, "peri.log"),
        LANGFUSE_PUBLIC_KEY: "", LANGFUSE_SECRET_KEY: "", OPENAI_API_KEY: "", ANTHROPIC_API_KEY: "",
      },
    });
    await tester.start();
    return tester;
  }

  function row(sql: string, id?: string) {
    const db = new DatabaseSync(database, { readOnly: true });
    try { return id === undefined ? db.prepare(sql).get() : db.prepare(sql).get(id); }
    finally { db.close(); }
  }

  function seedHistory(count: number) {
    const db = new DatabaseSync(database);
    for (let index = 0; index < count; index++) {
      const id = randomUUID();
      const messageId = randomUUID();
      const updated = new Date(Date.UTC(2026, 8, 3, 0, 0, index)).toISOString();
      db.prepare("INSERT INTO threads (id,title,cwd,created_at,updated_at,message_count) VALUES (?,?,?,?,?,1)")
        .run(id, `HISTORY_ROW_${String(index).padStart(3, "0")} 修复会话界面与中文长标题显示`, saved, updated, updated);
      db.prepare("INSERT INTO messages (message_id,thread_id,role,content) VALUES (?,?,'user',?)")
        .run(messageId, id, JSON.stringify({ role: "user", id: messageId, content: `RESTORED_ROW_${String(index).padStart(3, "0")}` }));
    }
    db.close();
  }

  async function captureLayout(ui: TmuxTester, name: string) {
    const capture = await ui.captureScreen();
    const output = path.join(process.env.E2E_RECORDINGS_DIR ?? path.join(PROJECT_ROOT, "e2e/recordings"), "history-panel-ui");
    fs.mkdirSync(output, { recursive: true });
    fs.writeFileSync(path.join(output, `${name}.json`), JSON.stringify(capture, null, 2));
    return capture;
  }

  function visibleHistoryRows(lines: string[]) {
    return lines.filter(line => /^\s*[>›]?\s*HISTORY_ROW_\d+/.test(line));
  }

  async function resizePanel(ui: TmuxTester, size: { cols: number; rows: number }) {
    await ui.resize(size);
    // tmux changes its buffer size before the application redraws. A complete
    // panel border at the new width proves that clicks use the resized frame.
    await expect.poll(async () => {
      const { lines } = await ui.captureScreen();
      return lines.some(line => /^─+$/.test(line) && line.length === size.cols)
        && lines.some(line => line.includes("Enter") && line.includes("Esc"));
    }, { timeout: 5_000 }).toBe(true);
  }

  it("lists upgraded history and previews a missing directory without binding it, including after reopening the upgraded database", async () => {
    for (let attempt = 0; attempt < 2; attempt++) {
      const ui = await launch();
      await ui.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
      await sendPrompt(ui, "/threads");
      await ui.waitForText("LEGACY_SAVED_PROJECT", { timeout: 10_000, interval: 100 });
      await ui.sendKey("tab");
      await ui.sendKey("tab");
      await ui.waitForText("LEGACY_DELETED_PROJECT", { timeout: 10_000, interval: 100 });
      await ui.sendKey("v");
      await ui.waitForText("LEGACY_READ_ONLY_MESSAGE", { timeout: 10_000, interval: 100 });
      expect(row("PRAGMA user_version")?.user_version).toBe(CURRENT_SCHEMA_VERSION);
      expect(row("SELECT COUNT(*) AS n FROM session_bindings WHERE thread_id = ?", missingId)?.n).toBe(0);
      expect(row("SELECT COUNT(*) AS n FROM session_bindings WHERE thread_id = ?", oldId)?.n).toBe(0);
      expect(row("SELECT frozen_context FROM threads WHERE id = ?", missingId)?.frozen_context).toBeNull();
      await ui.stop();
      tester = undefined;
    }
  });

  it.each(["-c", "-r"])("%s restores old history through the actual startup path", async flag => {
    const ui = await launch(flag === "-c" ? [flag] : [flag, oldId], flag === "-c" ? saved : directory);
    await ui.waitForText("LEGACY_RESTORED_MESSAGE", { timeout: 20_000, interval: 100 });
    expect(row("SELECT COUNT(*) AS n FROM session_bindings WHERE thread_id = ?", oldId)?.n).toBe(1);
    expect(row("SELECT frozen_context FROM threads WHERE id = ?", oldId)?.frozen_context).toBeTypeOf("string");
    expect(row("SELECT message_count FROM threads WHERE id = ?", oldId)?.message_count).toBe(1);
  });

  it("browsing to the end loads older pages beyond 50 sessions", async () => {
    const db = new DatabaseSync(database);
    for (let index = 0; index < 105; index++) {
      const id = randomUUID();
      const messageId = randomUUID();
      const updated = new Date(Date.UTC(2026, 8, 3, 0, 0, index)).toISOString();
      db.prepare("INSERT INTO threads (id,title,cwd,created_at,updated_at,message_count) VALUES (?,?,?, ?,?,1)")
        .run(id, `PAGED_HISTORY_${index}`, saved, updated, updated);
      db.prepare("INSERT INTO messages (message_id,thread_id,role,content) VALUES (?,?,'user',?)")
        .run(messageId, id, JSON.stringify({ role: "user", id: messageId, content: "page fixture" }));
    }
    db.close();
    const ui = await launch();
    await ui.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
    await sendPrompt(ui, "/threads");
    await ui.waitForText("PAGED_HISTORY_104", { timeout: 10_000, interval: 100 });
    await ui.sendKey("end");
    await ui.waitForText("100 sessions loaded", { timeout: 5_000, interval: 100 });
    await ui.sendKey("end");
    await ui.waitForText("106 sessions", { timeout: 5_000, interval: 100 });
    await ui.sendKey("end");
    await ui.waitForText("LEGACY_SAVED_PROJECT", { timeout: 5_000, interval: 100 });
    expect(row("SELECT COUNT(*) AS n FROM threads WHERE cwd = ? AND hidden = 0 AND message_count > 0", saved)?.n).toBe(106);
    expect(row("SELECT COUNT(*) AS n FROM session_bindings WHERE thread_id = ?", oldId)?.n).toBe(0);
  });

  it("keeps a compact stable list through preview, refresh, resize, wheel and click", async () => {
    seedHistory(32);
    const ui = await launch();
    await ui.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
    await sendPrompt(ui, "/threads");
    await ui.waitForText("HISTORY_ROW_031", { timeout: 10_000, interval: 100 });
    let capture = await captureLayout(ui, "wide-140x45");
    expect(visibleHistoryRows(capture.lines).length).toBeGreaterThanOrEqual(8);
    expect(capture.text.split("saved-project").length - 1).toBeLessThanOrEqual(2);
    expect(capture.lines.some(line => line.includes("Enter") && line.includes("Esc"))).toBe(true);

    await ui.sendKey("pagedown");
    const beforePreview = await captureLayout(ui, "after-page-down");
    const selectedRow = beforePreview.lines.find(line => /^\s*>\s*HISTORY_ROW_\d+/.test(line));
    expect(selectedRow, "PageDown must keep its selected row visible").toBeDefined();
    const previewIndex = selectedRow!.match(/HISTORY_ROW_(\d+)/)![1];
    const previewDb = new DatabaseSync(database);
    const previewMessage = previewDb.prepare("SELECT message_id FROM messages WHERE thread_id = (SELECT id FROM threads WHERE title LIKE ?)")
      .get(`HISTORY_ROW_${previewIndex} %`)!;
    previewDb.prepare("UPDATE messages SET content = ? WHERE message_id = ?").run(JSON.stringify({
      role: "user", id: previewMessage.message_id,
      content: `RESTORED_ROW_${previewIndex}\n` + Array.from({ length: 80 }, (_, index) => `Preview transcript line ${index}`).join("\n"),
    }), previewMessage.message_id);
    previewDb.close();
    await ui.sendKey("v");
    await ui.waitForText(`RESTORED_ROW_${previewIndex}`, { timeout: 5_000, interval: 100 });
    await captureLayout(ui, "preview");
    await ui.sendKey("pagedown");
    const scrolledPreview = await captureLayout(ui, "preview-scrolled");
    expect(scrolledPreview.text).not.toContain(`RESTORED_ROW_${previewIndex}`);
    expect(scrolledPreview.lines.some(line => /v.*back/.test(line) && line.includes("Esc"))).toBe(true);
    await ui.sendKey("v");
    await ui.waitForPattern(new RegExp(`>\\s*HISTORY_ROW_${previewIndex}`), { timeout: 5_000, interval: 100 });
    expect(visibleHistoryRows((await ui.captureScreen()).lines)[0]).toBe(visibleHistoryRows(beforePreview.lines)[0]);
    await ui.sendKey("home");
    await ui.sendKey("down");
    await ui.waitForPattern(/>\s*HISTORY_ROW_030/, { timeout: 5_000, interval: 100 });
    await ui.sendKey("d");
    await ui.waitForText("confirm", { timeout: 5_000, interval: 100 });
    await captureLayout(ui, "delete-confirm");
    await ui.sendKey("v"); // Existing non-Enter key cancels delete confirmation.

    // A periodic refresh may insert ahead of the cursor; selection must keep its thread ID.
    const db = new DatabaseSync(database);
    const id = randomUUID();
    const messageId = randomUUID();
    db.prepare("INSERT INTO threads (id,title,cwd,created_at,updated_at,message_count) VALUES (?,?,?,'2026-09-04T00:00:00Z','2026-09-04T00:00:00Z',1)")
      .run(id, "NEW_HEAD_FROM_REFRESH", saved);
    db.prepare("INSERT INTO messages (message_id,thread_id,role,content) VALUES (?,?,'user',?)")
      .run(messageId, id, JSON.stringify({ role: "user", id: messageId, content: "new head" }));
    db.close();
    await ui.waitForText("NEW_HEAD_FROM_REFRESH", { timeout: 6_000, interval: 100 });
    await ui.waitForPattern(/>\s*HISTORY_ROW_030/, { timeout: 5_000, interval: 100 });

    for (const size of [{ cols: 80, rows: 24 }, { cols: 60, rows: 18 }]) {
      await resizePanel(ui, size);
      await ui.waitForPattern(/>\s*HISTORY_ROW_030/, { timeout: 5_000, interval: 100 });
      capture = await captureLayout(ui, `narrow-${size.cols}x${size.rows}`);
      expect(visibleHistoryRows(capture.lines).length).toBeGreaterThanOrEqual(3);
      expect(capture.lines.some(line => line.includes("Enter") && line.includes("Esc"))).toBe(true);
      expect(capture.lines.some(line => /ID:\s*[a-f0-9]{8}/.test(line))).toBe(true);
    }
    await resizePanel(ui, { cols: 140, rows: 45 });
    await ui.waitForPattern(/>\s*HISTORY_ROW_030/, { timeout: 5_000, interval: 100 });
    capture = await ui.captureScreen();
    const before = visibleHistoryRows(capture.lines)[0];
    const headerRow = capture.lines.findIndex(line => line.includes("Project"));
    const footerRow = capture.lines.findIndex(line => line.includes("Enter") && line.includes("Esc"));
    const bodyRow = capture.lines.findIndex(line => /^\s*[>›]?\s*HISTORY_ROW_\d+/.test(line));
    // The tester helper emits 4/5, not SGR wheel 64/65. Send the proper sequence directly.
    execFileSync("tmux", ["send-keys", "-l", "-t", ui.getSessionName(), `\x1b[<65;10;${bodyRow + 1}M`]);
    await expect.poll(async () => visibleHistoryRows((await ui.captureScreen()).lines)[0], { timeout: 5_000 }).not.toBe(before);
    capture = await captureLayout(ui, "after-wheel");
    expect(capture.lines.findIndex(line => line.includes("Project"))).toBe(headerRow);
    expect(capture.lines.findIndex(line => line.includes("Enter") && line.includes("Esc"))).toBe(footerRow);
    const clickedRow = capture.lines.findIndex(line => /^\s*[>›]?\s*HISTORY_ROW_\d+/.test(line));
    const clickedIndex = capture.lines[clickedRow].match(/HISTORY_ROW_(\d+)/)![1];
    await ui.sendMouse({ type: "click", button: "left", position: { x: 8, y: clickedRow } });
    await ui.waitForText(`RESTORED_ROW_${clickedIndex}`, { timeout: 10_000, interval: 100 });
    await captureLayout(ui, "after-click-restore");
  });

  it("keeps Chinese actions and selected details readable at narrow widths", async () => {
    seedHistory(32);
    const config = JSON.parse(fs.readFileSync(settings, "utf8"));
    config.config.language = "zh-CN";
    fs.writeFileSync(settings, JSON.stringify(config));
    const ui = await launch();
    await ui.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
    await sendPrompt(ui, "/threads");
    await ui.waitForText("HISTORY_ROW_031", { timeout: 20_000, interval: 100 });
    await captureLayout(ui, "chinese-wide-140x45");
    for (const size of [{ cols: 80, rows: 24 }, { cols: 60, rows: 18 }]) {
      await resizePanel(ui, size);
      await ui.waitForPattern(/>\s*HISTORY_ROW_031/, { timeout: 5_000, interval: 100 });
      const capture = await captureLayout(ui, `chinese-narrow-${size.cols}x${size.rows}`);
      expect(visibleHistoryRows(capture.lines).length).toBeGreaterThanOrEqual(3);
      expect(capture.text).toContain("项目");
      expect(capture.lines.some(line => line.includes("Enter") && line.includes("Esc"))).toBe(true);
      expect(capture.lines.some(line => /ID：\s*[a-f0-9]{8}/.test(line))).toBe(true);
    }
  });

  it("keeps an empty compact panel usable in a short terminal", async () => {
    const db = new DatabaseSync(database);
    db.exec("DELETE FROM messages; DELETE FROM threads;");
    db.close();
    const ui = await launch();
    await ui.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
    await ui.resize({ cols: 60, rows: 18 });
    await sendPrompt(ui, "/threads");
    await ui.waitForText("No conversations yet", { timeout: 10_000, interval: 100 });
    const capture = await captureLayout(ui, "empty-60x18");
    expect(capture.lines.some(line => line.includes("Enter") && line.includes("Esc"))).toBe(true);
    expect(capture.text).not.toContain("id:");
    await ui.sendKey("escape");
    await expect.poll(() => ui.getScreenText(), { timeout: 5_000 }).not.toContain("Threads");
  });

  it("does not delete the neighboring session when a confirmed target disappears during refresh", async () => {
    seedHistory(2);
    const ui = await launch();
    await ui.waitForText("AI operating system", { timeout: 20_000, interval: 100 });
    await sendPrompt(ui, "/threads");
    await ui.waitForPattern(/>\s*HISTORY_ROW_001/, { timeout: 10_000, interval: 100 });
    await ui.sendKey("d");
    await ui.waitForText("confirm", { timeout: 5_000, interval: 100 });
    const db = new DatabaseSync(database);
    const targetId = db.prepare("SELECT id FROM threads WHERE title LIKE 'HISTORY_ROW_001 %'").get()!.id;
    db.prepare("DELETE FROM messages WHERE thread_id = ?").run(targetId);
    db.prepare("DELETE FROM threads WHERE id = ?").run(targetId);
    db.close();
    await expect.poll(() => ui.getScreenText(), { timeout: 6_000 }).not.toContain("HISTORY_ROW_001");
    await ui.sendKey("enter");
    await ui.waitForText("Enter resume", { timeout: 5_000, interval: 100 });
    await ui.sendKey("v");
    await ui.waitForText("RESTORED_ROW_000", { timeout: 5_000, interval: 100 });
    expect(row("SELECT COUNT(*) AS n FROM threads WHERE title LIKE 'HISTORY_ROW_000 %'")?.n).toBe(1);
    expect(row("SELECT COUNT(*) AS n FROM session_bindings WHERE thread_id = (SELECT id FROM threads WHERE title LIKE 'HISTORY_ROW_000 %')")?.n).toBe(0);
  });
});
