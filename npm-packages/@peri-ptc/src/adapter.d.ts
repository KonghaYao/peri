import type { ToolCallErrorCode, Tools } from "./types.js";

export declare class ToolCallError extends Error {
  readonly code: ToolCallErrorCode;
  constructor(code: ToolCallErrorCode, message: string);
}

export interface PtcAdapter {
  handleMessage(message: unknown): Promise<void>;
  tools: Tools;
}

export declare function createPtcAdapter(
  write: (line: string) => void,
): PtcAdapter;

export declare function startPtcAdapter(): PtcAdapter;
