// Vendored from peri-canvas@0.1.0 (packages/canvas/src/hooks.ts), heavily reduced.
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// See PROVENANCE.md. Internal use only; do not redistribute without legal review.
//
// Upstream hooks also carried host-integration APIs (global bridge, persisted
// canvas state, HTTP fetches, imperative state/action). peri-cf renders OpenUI
// inside a chat message and owns interaction through the renderer, so those
// APIs are removed.
// What remains is a process-local theme store: the host pushes CSS-derived
// colors in, canvas primitives read them through `useHostTheme()`.
import { useSyncExternalStore } from "react";
import type { CanvasPalette, CanvasTokens } from "./tokens";
import { buildHostTokens, type CanvasHostThemeOverrides } from "./tokens";

export interface CanvasHostTheme extends CanvasTokens {
  readonly kind: string;
  readonly tokens: CanvasTokens;
  readonly palette: CanvasPalette;
}

export interface HostThemeInput extends CanvasHostThemeOverrides {
  readonly kind?: string;
}

function assembleHostTheme(
  kind: string,
  built: { tokens: CanvasTokens; palette: CanvasPalette },
): CanvasHostTheme {
  return { ...built.tokens, kind, tokens: built.tokens, palette: built.palette };
}

function buildTheme(input: HostThemeInput | undefined): CanvasHostTheme {
  const kind = input?.kind && input.kind.length > 0 ? input.kind : "light";
  return assembleHostTheme(
    kind,
    buildHostTokens(kind, {
      primary: input?.primary,
      editorBackground: input?.editorBackground,
      editorForeground: input?.editorForeground,
    }),
  );
}

let lastInput: HostThemeInput = { kind: "light" };
let currentTheme = buildTheme(lastInput);
const listeners = new Set<() => void>();

/**
 * 宿主推送主题色。仅在输入真正变化时通知订阅者，避免每次 DOM mutation
 * 都触发整棵 OpenUI 树重渲染。
 */
export function setHostTheme(input: HostThemeInput): void {
  if (
    input.kind === lastInput.kind &&
    input.primary === lastInput.primary &&
    input.editorBackground === lastInput.editorBackground &&
    input.editorForeground === lastInput.editorForeground
  ) {
    return;
  }
  lastInput = input;
  currentTheme = buildTheme(input);
  for (const notify of listeners) notify();
}

export function getHostTheme(): CanvasHostTheme {
  return currentTheme;
}

function subscribe(onStoreChange: () => void): () => void {
  listeners.add(onStoreChange);
  return () => listeners.delete(onStoreChange);
}

export function useHostTheme(): CanvasHostTheme {
  return useSyncExternalStore(
    subscribe,
    getHostTheme,
    getHostTheme,
  );
}
