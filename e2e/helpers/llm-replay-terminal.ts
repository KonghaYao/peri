import headless from "@xterm/headless";
import unicode11 from "@xterm/addon-unicode11";
import type { Terminal } from "@xterm/headless";

/** Match the CJK and emoji cell widths used by Peri; Unicode 11 is not full grapheme clustering. */
export function createReplayTerminal(cols: number, rows: number): Terminal {
  const terminal = new headless.Terminal({ cols, rows, scrollback: 5000, allowProposedApi: true });
  terminal.loadAddon(new unicode11.Unicode11Addon());
  terminal.unicode.activeVersion = "11";
  return terminal;
}
