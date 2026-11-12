import { noteCount, resetNotes } from "./notes.ts";

/**
 * Demo 的可变状态与资源 URI。
 *
 * 与 notes 一样，状态必须留在模块作用域：stateless handler 每次请求都会重建。
 */

export type Theme = "light" | "dark";

export const STATE_URI = "demo://state";
export const REPORT_URI = "demo://report";
export const NOTES_INDEX_URI = "demo://notes";

export let theme: Theme = "light";
export let revision = 1;
export let taskRuns = 0;

function now(): string {
  return new Date().toISOString();
}

export function bumpRevision(): void {
  revision += 1;
}

export function setTheme(next: Theme): void {
  theme = next;
}

export function incrementTaskRuns(): number {
  taskRuns += 1;
  return taskRuns;
}

export function statePayload() {
  return {
    theme,
    revision,
    noteCount: noteCount(),
    taskRuns,
    updatedAt: now(),
  };
}

export function resetState(): void {
  resetNotes();
  theme = "light";
  taskRuns = 0;
  bumpRevision();
}
