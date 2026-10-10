/** 把子进程输出流原样转发到多个目标（终端与日志文件），供 local-preview 同时落盘与展示。 */
export interface OutputTarget {
  write(chunk: Uint8Array): unknown;
}

export async function relayOutput(
  source: ReadableStream<Uint8Array> | undefined,
  ...targets: OutputTarget[]
): Promise<void> {
  if (!source) return;
  const reader = source.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (!value) continue;
      for (const target of targets) target.write(value);
    }
  } finally {
    reader.releaseLock();
  }
}
