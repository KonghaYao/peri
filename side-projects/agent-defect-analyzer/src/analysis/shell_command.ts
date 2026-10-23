/**
 * Pure parsing for shell `sleep` waiting. No database or filesystem access.
 *
 * The parser is deliberately conservative: only a simple command whose effective
 * first token is `sleep` counts. Wrapper-embedded sleeps (`bash -c 'sleep 5'`,
 * `xargs sleep`, `sudo sleep`) are reported separately as a sensitivity check.
 */

export type SleepShape = "standalone" | "compound" | "loop" | "variable";
export type WaitKind = "pure" | "noop_only" | "other_command";

export interface SleepCommandAnalysis {
  segments: number;
  sleepSegments: number;
  /** Sum of literal sleep seconds in one tool call. Loop repetitions are not expanded. */
  seconds: number;
  variableSleep: boolean;
  shape: SleepShape | null;
  waitKind: WaitKind | "none";
}

const WRAPPER_PREFIXES = new Set(["time", "nohup", "command", "exec", "builtin", "env"]);
const CONTROL_KEYWORDS = new Set([
  "while", "until", "if", "then", "do", "else", "elif", "fi", "done",
  "case", "esac", "function", "!", "{", "}", "(", ")",
]);
const NOOP_COMMANDS = new Set(["echo", "printf", "true", "false", ":", "date"]);
const SLEEP_UNIT_SECONDS: Record<string, number> = { "": 1, s: 1, m: 60, h: 3600, d: 86400 };

/** Split a shell command on top-level separators, ignoring quoted separators and redirections. */
export function splitShellSegments(command: string): string[] {
  const segments: string[] = [];
  let current = "";
  let single = false;
  let double = false;
  let escaped = false;
  for (let index = 0; index < command.length; index++) {
    const character = command[index]!;
    if (escaped) { current += character; escaped = false; continue; }
    if (character === "\\" && !single) { current += character; escaped = true; continue; }
    if (character === "'" && !double) { single = !single; current += character; continue; }
    if (character === '"' && !single) { double = !double; current += character; continue; }
    if (single || double) { current += character; continue; }
    if (character === ";" || character === "\n" || character === "|") {
      segments.push(current); current = ""; continue;
    }
    if (character === "&") {
      const previous = command[index - 1];
      const next = command[index + 1];
      // Keep redirections such as 2>&1, &>file, and >&2 inside one simple command.
      if (previous === ">" || previous === "<" || next === ">") { current += character; continue; }
      segments.push(current); current = ""; continue;
    }
    current += character;
  }
  segments.push(current);
  return segments.map((segment) => segment.trim()).filter((segment) => segment.length > 0);
}

/** Split one simple command into tokens, removing quote characters. */
export function tokenizeShellSegment(segment: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let started = false;
  let single = false;
  let double = false;
  let escaped = false;
  for (const character of segment) {
    if (escaped) { current += character; escaped = false; started = true; continue; }
    if (character === "\\" && !single) { escaped = true; started = true; continue; }
    if (character === "'" && !double) { single = !single; started = true; continue; }
    if (character === '"' && !single) { double = !double; started = true; continue; }
    if (!single && !double && /\s/.test(character)) {
      if (started) { tokens.push(current); current = ""; started = false; }
      continue;
    }
    current += character; started = true;
  }
  if (started) tokens.push(current);
  return tokens;
}

/** Index of the effective first command token, skipping wrappers, assignments, and control keywords. */
export function resolveFirstCommand(tokens: string[]): number {
  let index = 0;
  while (index < tokens.length) {
    const token = tokens[index]!;
    if (WRAPPER_PREFIXES.has(token)) {
      index++;
      while (index < tokens.length && tokens[index]!.startsWith("-")) index++;
      continue;
    }
    if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(token)) { index++; continue; }
    if (CONTROL_KEYWORDS.has(token)) { index++; continue; }
    break;
  }
  return index < tokens.length ? index : -1;
}

export function parseSleepSeconds(token: string | undefined): number | null {
  if (!token) return null;
  const match = /^(\d+(?:\.\d+)?)([smhd]?)$/.exec(token);
  if (!match) return null;
  return Number(match[1]) * (SLEEP_UNIT_SECONDS[match[2]!] ?? 1);
}

function classifyWaitKind(segments: string[], sleepSegments: number): SleepCommandAnalysis["waitKind"] {
  if (sleepSegments === 0) return "none";
  let nonSleepSegments = 0;
  for (const segment of segments) {
    const tokens = tokenizeShellSegment(segment);
    const index = resolveFirstCommand(tokens);
    if (index < 0 || tokens[index] === "sleep") continue;
    nonSleepSegments++;
    // echo "$(pgrep ...)" still observes state through substitution.
    if (!NOOP_COMMANDS.has(tokens[index]!) || /\$\(|`/.test(segment)) return "other_command";
  }
  return nonSleepSegments === 0 ? "pure" : "noop_only";
}

export function analyzeSleepCommand(command: string): SleepCommandAnalysis {
  const segments = splitShellSegments(command);
  let sleepSegments = 0;
  let seconds = 0;
  let variableSleep = false;
  for (const segment of segments) {
    const tokens = tokenizeShellSegment(segment);
    const index = resolveFirstCommand(tokens);
    if (index < 0 || tokens[index] !== "sleep") continue;
    sleepSegments++;
    const parsed = parseSleepSeconds(tokens[index + 1]);
    if (parsed === null) variableSleep = true;
    else seconds += parsed;
  }
  let shape: SleepShape | null = null;
  if (sleepSegments > 0) {
    if (variableSleep && seconds === 0) shape = "variable";
    else if (/\b(while|until|for)\b/.test(command)) shape = "loop";
    else if (segments.length === 1) shape = "standalone";
    else shape = "compound";
  }
  return {
    segments: segments.length,
    sleepSegments,
    seconds,
    variableSleep,
    shape,
    waitKind: classifyWaitKind(segments, sleepSegments),
  };
}

/** Extract a shell command from common tool argument shapes. */
export function extractCommandArgument(args: unknown): string | null {
  if (typeof args === "string") return args;
  if (!args || typeof args !== "object" || Array.isArray(args)) return null;
  const record = args as Record<string, unknown>;
  for (const key of ["command", "cmd", "script", "shell_command"]) {
    if (typeof record[key] === "string") return record[key] as string;
  }
  return null;
}
