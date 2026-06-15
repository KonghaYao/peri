import { Terminal } from 'xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';

const params = new URLSearchParams(location.search);
const shell = params.get('shell') || '';

const term = new Terminal({
  cursorBlink: true,
  fontSize: 14,
  fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", Menlo, monospace',
  theme: {
    background: '#1a1a2e',
    foreground: '#e0e0e0',
    cursor: '#e0e0e0',
    selectionBackground: 'rgba(100, 100, 255, 0.3)',
    black: '#1a1a2e',
    red: '#ff6b6b',
    green: '#51cf66',
    yellow: '#ffd43b',
    blue: '#74c0fc',
    magenta: '#da77f2',
    cyan: '#63e6be',
    white: '#e0e0e0',
  },
});

const fitAddon = new FitAddon();
const webLinksAddon = new WebLinksAddon();
term.loadAddon(fitAddon);
term.loadAddon(webLinksAddon);
term.open(document.getElementById('terminal')!);
fitAddon.fit();

const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
const wsUrl = `${protocol}//${location.host}/ws?shell=${encodeURIComponent(shell)}&cols=${term.cols}&rows=${term.rows}`;
const ws = new WebSocket(wsUrl);
ws.binaryType = 'arraybuffer';

term.onData((data) => {
  if (ws.readyState === WebSocket.OPEN) {
    ws.send(data);
  }
});

ws.onmessage = (event) => {
  if (typeof event.data === 'string') {
    term.write(event.data);
  }
};

ws.onclose = () => {
  term.write('\r\n\x1b[33m[connection closed]\x1b[0m\r\n');
};

window.addEventListener('resize', () => {
  fitAddon.fit();
  if (ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }));
  }
});
