import { mkdir, lstat, readFile, realpath } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import path from "node:path";

export interface IsolatedReplayWorld { root: string; home: string; work: string }

function inside(root: string, candidate: string): boolean {
  const relative = path.relative(root, candidate);
  return relative !== "" && relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

function assertWorldLayout(world: IsolatedReplayWorld): void {
  if (!path.isAbsolute(world.root) || !path.isAbsolute(world.home) || !path.isAbsolute(world.work)
    || !inside(world.root, world.home) || !inside(world.root, world.work)) {
    throw new Error("replay isolation requires absolute HOME/work below its owned temporary root");
  }
}

/** Inspect each existing component before mkdir/open can follow a link out of the owned directory. */
async function assertUnredirected(root: string, candidate: string): Promise<void> {
  if (candidate !== root && !inside(root, candidate)) throw new Error("replay isolation rejects an escaped persistence path");
  let current = root;
  for (const component of ["", ...path.relative(root, candidate).split(path.sep).filter(Boolean)]) {
    current = path.join(current, component);
    try {
      if ((await lstat(current)).isSymbolicLink()) throw new Error("replay isolation rejects a redirected persistence path");
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
      throw error;
    }
  }
}

/** Shared by raw ACP fixtures as well as the TUI; lexical prefix checks accept HOME/../outside. */
export async function assertReplayStorePath(home: string, databasePath: string): Promise<void> {
  if (!path.isAbsolute(home) || !path.isAbsolute(databasePath) || !inside(home, databasePath)) {
    throw new Error("replay isolation requires the session store below isolated HOME");
  }
  for (const suffix of ["", "-wal", "-shm", "-journal"]) await assertUnredirected(home, `${databasePath}${suffix}`);
  await assertUnredirected(home, path.join(home, ".peri", "logs"));
}

/** Session storage is explicit at every process entry, even when the product default changes. */
export function replayStorePath(world: IsolatedReplayWorld): string {
  return path.join(world.home, ".peri", "threads", "threads.db");
}

export function replayIsolationEnvironment(world: IsolatedReplayWorld): Record<string, string> {
  return {
    HOME: world.home,
    USERPROFILE: world.home,
    XDG_CONFIG_HOME: path.join(world.home, "config"),
    XDG_CACHE_HOME: path.join(world.home, "cache"),
    XDG_DATA_HOME: path.join(world.home, "data"),
    XDG_STATE_HOME: path.join(world.home, "state"),
    XDG_RUNTIME_DIR: path.join(world.home, "runtime"),
    TMPDIR: path.join(world.home, "tmp"),
    TMP: path.join(world.home, "tmp"),
    TEMP: path.join(world.home, "tmp"),
    PERI_DB_PATH: replayStorePath(world),
  };
}

/** Config callbacks and per-client overrides cannot redirect persistence outside the owned world. */
export function assertReplayIsolation(world: IsolatedReplayWorld, env: Record<string, string>, extraArgs: string[] = []): void {
  assertWorldLayout(world);
  for (const [key, value] of Object.entries(replayIsolationEnvironment(world))) {
    if (env[key] !== value) throw new Error(`replay isolation rejects ${key} override`);
  }
  for (const key of ["PERI_SESSION_STORE", "PERI_CONFIG_FILE", "BASH_ENV", "ENV", "ANTHROPIC_API_KEY", "OPENAI_API_KEY"]) {
    if (env[key] !== undefined) throw new Error(`replay isolation rejects ${key} override`);
  }
  if (env.RUST_LOG_FILE !== undefined && (!path.isAbsolute(env.RUST_LOG_FILE) || !inside(world.home, env.RUST_LOG_FILE))) {
    throw new Error("replay isolation rejects RUST_LOG_FILE override");
  }
  const reserved = /^(?:--db-path|--dbPath|--db-url|--session-store|--sessionStore|--session-store-engine|--sessionStoreEngine|--session-store-token-env|--sessionStoreTokenEnv|--config-file|--configFile|--settings|--no-session-persistence)(?:=|$)/;
  if (extraArgs.some((argument) => reserved.test(argument))) throw new Error("replay isolation rejects storage/config CLI override");
}

async function assertSettingsEnvironment(world: IsolatedReplayWorld, file: string, claude = false): Promise<void> {
  await assertUnredirected(world.root, file);
  let settings: any;
  try { settings = JSON.parse(await readFile(file, "utf8")); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT" || error instanceof SyntaxError) return;
    throw error;
  }
  const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
  const injected = !claude && object(settings?.config?.env) ? settings.config.env : settings?.env;
  if (!object(injected)) return;
  const env = Object.fromEntries(Object.entries(injected).filter((entry): entry is [string, string] => typeof entry[1] === "string"));
  assertReplayIsolation(world, { ...replayIsolationEnvironment(world), ...env });
  if (env.RUST_LOG_FILE) await assertUnredirected(world.root, env.RUST_LOG_FILE);
}

export async function prepareReplayIsolation(world: IsolatedReplayWorld): Promise<void> {
  assertWorldLayout(world);
  await assertUnredirected(world.root, world.root);
  const directories = new Set([world.work, path.join(world.home, ".peri", "threads"), path.join(world.home, ".peri", "logs"),
    ...Object.entries(replayIsolationEnvironment(world)).filter(([key]) => key !== "PERI_DB_PATH").map(([, value]) => value)]);
  for (const directory of directories) {
    await assertUnredirected(world.root, directory);
    await mkdir(directory, { recursive: true, mode: 0o700 });
    if ((await lstat(directory)).isSymbolicLink() || !inside(await realpath(world.root), await realpath(directory))) {
      throw new Error("replay isolation rejects a redirected runtime directory");
    }
  }
  await assertReplayStorePath(world.home, replayStorePath(world));
  await assertSettingsEnvironment(world, path.join(world.home, ".peri", "settings.json"));
  await assertSettingsEnvironment(world, path.join(world.home, ".claude", "settings.json"), true);
  await assertSettingsEnvironment(world, path.join(world.work, ".peri", "settings.json"));
}

export interface ReplayStoredMessage { thread_id: string; role: string; content: string }
export interface ReplayStoreEvidence {
  path: string;
  integrity: string;
  threads: Array<{ id: string; parent_thread_id: string | null; work_state: "idle" | "working" | "stopped" }>;
  messages: ReplayStoredMessage[];
}

/** Read-only evidence: fixtures never insert session rows or relocate cwd in a recorded database. */
export function readReplayStore(world: IsolatedReplayWorld): ReplayStoreEvidence {
  const databasePath = replayStorePath(world);
  if (!inside(world.root, databasePath)) throw new Error("replay store escaped the temporary world");
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    const integrity = (db.prepare("PRAGMA quick_check").get() as { quick_check: string }).quick_check;
    return {
      path: databasePath,
      integrity,
      threads: db.prepare("SELECT id, parent_thread_id, work_state FROM threads ORDER BY id").all() as ReplayStoreEvidence["threads"],
      messages: db.prepare("SELECT thread_id, role, content FROM messages ORDER BY rowid").all() as unknown as ReplayStoredMessage[],
    };
  } finally { db.close(); }
}
