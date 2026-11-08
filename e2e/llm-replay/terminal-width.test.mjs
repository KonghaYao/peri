import assert from 'node:assert/strict';
import test from 'node:test';
import { tsImport } from 'tsx/esm/api';

const { createReplayTerminal } = await tsImport('../helpers/llm-replay-terminal.ts', import.meta.url);

const write = (terminal, bytes) => new Promise((resolve) => terminal.write(bytes, resolve));

test('replay terminal assigns CJK, rocket and combining text their actual cell widths', async () => {
  const terminal = createReplayTerminal(64, 24);
  try {
    await write(terminal, '中文🚀 e\u0301');
    assert.equal(terminal.unicode.activeVersion, '11');
    assert.equal(terminal.buffer.active.cursorX, 8);
    const line = terminal.buffer.active.getLine(0);
    assert.equal(line.translateToString(true), '中文🚀 e\u0301');
    assert.deepEqual(Array.from({ length: 8 }, (_, col) => line.getCell(col).getWidth()), [2, 0, 2, 0, 2, 0, 1, 1]);
  } finally { terminal.dispose(); }
});

test('absolute cursor updates after a wide emoji leave no stale characters across resize', async () => {
  const terminal = createReplayTerminal(120, 40);
  try {
    for (const [cols, rows] of [[120, 40], [64, 24]]) {
      terminal.resize(cols, rows);
      await write(terminal, '\x1b[2J\x1b[H中文🚀 stale');
      await write(terminal, '\x1b[1;7H value 34\x1b[K');
      assert.equal(terminal.buffer.active.getLine(0).translateToString(true), '中文🚀 value 34');
      assert.equal(terminal.buffer.active.cursorX, 15);
    }
  } finally { terminal.dispose(); }
});
