import { StringDecoder } from 'node:string_decoder';

/** 只观察协议终结事实；不修补、补发、合并或转换任何录制字节。 */
export function createProtocolObserver(target, headers) {
  const pathname = new URL(target).pathname;
  const protocol = /\/messages$/.test(pathname) ? 'anthropic'
    : /\/chat\/completions$/.test(pathname) ? 'openai-chat'
      : /\/responses$/.test(pathname) ? 'openai-responses' : 'unknown';
  const sse = String(headers['content-type'] ?? '').split(';')[0].trim().toLowerCase() === 'text/event-stream';
  const observable = sse && !headers['content-encoding'] && protocol !== 'unknown';
  const decoder = new StringDecoder('utf8');
  let line = '', skipLine = false, pendingCR = false, event = '', data = [], marker;
  function finishLine() {
    if (skipLine) { skipLine = false; line = ''; return; }
    if (line === '') {
      if (data.length) {
        const payload = data.join('\n');
        if (protocol === 'openai-chat' && payload === '[DONE]') marker = '[DONE]';
        if (protocol === 'anthropic' && ['message_stop', 'error'].includes(event)) marker = event;
        if (protocol === 'openai-responses' && ['response.completed', 'response.failed', 'response.incomplete', 'error'].includes(event)) marker = event;
        if (!event && protocol !== 'openai-chat') {
          try {
            const type = JSON.parse(payload).type;
            if ((protocol === 'anthropic' && ['message_stop', 'error'].includes(type))
                || (protocol === 'openai-responses' && ['response.completed', 'response.failed', 'response.incomplete', 'error'].includes(type))) marker = type;
          } catch { /* 不完整或非 JSON data 不形成可识别终态。 */ }
        }
      }
      event = ''; data = [];
    } else if (line.startsWith('event:')) event = line.slice(6).replace(/^ /, '');
    else if (line.startsWith('data:')) {
      // 终结消息很小；不为任意大模型输出构建第二份无界副本。
      if (data.reduce((sum, part) => sum + part.length, 0) < 262144) data.push(line.slice(5).replace(/^ /, ''));
    }
    line = '';
  }
  return {
    feed(bytes) {
      if (!observable) return;
      for (const char of decoder.write(bytes)) {
        if (pendingCR) { pendingCR = false; if (char === '\n') continue; }
        if (char === '\r' || char === '\n') { finishLine(); pendingCR = char === '\r'; }
        else if (!skipLine) {
          line += char;
          if (line.length > 262144) { line = ''; skipLine = true; }
        }
      }
    },
    snapshot() {
      return {
        protocol,
        basis: 'captured_body',
        terminalMarker: !sse ? 'not_applicable' : !observable ? 'unrecognized' : marker ? 'seen' : 'not_seen',
        ...(marker ? { marker } : {}),
      };
    },
  };
}
